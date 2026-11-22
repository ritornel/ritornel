//! The in-memory transport: a session joined to a pair of channels, for
//! tests. It carries the same text a socket does, so a test through it
//! still encodes and decodes every frame. The desktop app never uses it.
//! See "Transport-neutral dispatcher" in docs/internals/contract.md.

use std::sync::Arc;

use ritornel_contract::SessionKind;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::dispatch::Core;
use crate::transport::Wire;

/// How many frames wait in a channel before its sender does.
pub(crate) const WAITING: usize = 16;

/// The server's ends of the two channels.
struct Channels {
    from_client: Receiver<String>,
    to_client: Sender<String>,
}

impl Wire for Channels {
    async fn recv(&mut self) -> Option<String> {
        self.from_client.recv().await
    }

    async fn send(&mut self, text: String) -> bool {
        self.to_client.send(text).await.is_ok()
    }

    /// Dropping the channel is all the goodbye there is.
    async fn close(&mut self) {}
}

/// Opens a session of this kind and returns its two ends: where the client
/// sends frames, and where it receives them. The session ends when the
/// client drops the first, and the second closes when the server stops.
pub(crate) fn attach(core: &Arc<Core>, kind: SessionKind) -> (Sender<String>, Receiver<String>) {
    let (to_server, from_client) = mpsc::channel::<String>(WAITING);
    let (to_client, from_server) = mpsc::channel(WAITING);
    // A server that is stopping, or full, opens no session. The channels
    // are dropped here, and the client finds both closed.
    let _ = core.run(
        kind,
        None,
        Channels {
            from_client,
            to_client,
        },
    );
    (to_server, from_server)
}
