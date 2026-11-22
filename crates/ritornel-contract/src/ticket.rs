//! The socket ticket: how a client that cannot put a credential in a header
//! opens the socket. See "Query-string credentials and origin checks" in
//! docs/internals/contract.md, and "Local credentials, sessions, and tokens"
//! in docs/internals/linking.md.
//!
//! A page cannot set a header on a WebSocket upgrade, so the desktop's page
//! cannot present the credential as a native client does. A program that
//! holds the credential trades it for a ticket, with `POST`
//! [`route::TICKET`](crate::route::TICKET) and the credential in an
//! `Authorization` header, and the socket is then opened with the ticket in
//! the query string:
//!
//! ```text
//! GET /v1/ws?ticket=<ticket>
//! ```
//!
//! A ticket is never the session token. The server keeps it in memory only,
//! it opens one socket, and it stops working [`SocketTicket::expires_in_ms`]
//! after it was made whether it was used or not. The session it opens is of
//! the kind the credential holds.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The name of the query parameter that carries a ticket on
/// [`route::WS`](crate::route::WS).
pub const TICKET_PARAM: &str = "ticket";

/// The body of [`route::TICKET`](crate::route::TICKET): a ticket that opens
/// one socket.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SocketTicket {
    /// The ticket: what goes in the query string of the socket's URL. It
    /// works once.
    pub ticket: String,
    /// How long the ticket works for, counted from when the server made
    /// it. A client opens the socket at once and asks for a new ticket
    /// each time it connects.
    pub expires_in_ms: u64,
}

/// The ticket opens a socket, so it is not printed.
impl fmt::Debug for SocketTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketTicket")
            .field("ticket", &"<redacted>")
            .field("expires_in_ms", &self.expires_in_ms)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_ticket_is_its_text_and_how_long_it_works_for() {
        let ticket = SocketTicket {
            ticket: "0f".repeat(32),
            expires_in_ms: 10_000,
        };
        let wire = json!({ "ticket": "0f".repeat(32), "expiresInMs": 10_000 });
        assert_eq!(serde_json::to_value(&ticket).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<SocketTicket>(wire).unwrap(),
            ticket
        );
    }

    #[test]
    fn debug_does_not_print_the_ticket() {
        const SENTINEL: &str = "SENTINEL_SOCKET_TICKET_0001";
        let ticket = SocketTicket {
            ticket: SENTINEL.to_owned(),
            expires_in_ms: 10_000,
        };
        for printed in [format!("{ticket:?}"), format!("{ticket:#?}")] {
            assert!(!printed.contains(SENTINEL), "{printed}");
            assert!(printed.contains("<redacted>"), "{printed}");
        }
    }
}
