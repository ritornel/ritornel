//! The bounds a running server keeps: how much it serves at once, and how
//! long it waits. Each is here because without it something outside the
//! server could make it wait or grow for ever.

use std::time::Duration;

/// How many HTTP connections are served at once. A connection costs a file
/// descriptor and memory before any credential is checked, so their number
/// has a bound. One owner's programs need a handful: a page asks for a few
/// covers at a time, and each program makes one request to find the
/// server. A connection that becomes a socket stops counting here and
/// counts as a session.
pub const MAX_CONNECTIONS: usize = 128;

/// How many sessions are open at once, over sockets and in memory. One
/// owner has a session for each window, phone, command and agent. With
/// [`MAX_CONNECTIONS`] it keeps the server well inside the 256 file
/// descriptors a process on macOS gets by default.
pub const MAX_SESSIONS: usize = 64;

/// How long a connection has to send the whole head of a request, counted
/// from when the server starts waiting for one. It is also how long a
/// connection may sit between requests, so one that says nothing is closed
/// after this too. A program on this machine or this network needs
/// milliseconds.
pub const HEADER_WAIT: Duration = Duration::from_secs(10);

/// How long a stop waits for requests in flight and for transports to end
/// before it ends them itself. A program that owns the server kills it 15
/// seconds after asking it to stop (docs/internals/server.md, "Shutdown and
/// bounds"), and a stop spends this at most twice.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How long a socket ticket works for, counted from when it was made. A
/// program asks for one and opens its socket at once, on this machine, so
/// it is used within milliseconds. This is long enough for a program that
/// is slow to start, and short enough that a ticket which leaked is worth
/// nothing by the time anyone reads it.
pub const TICKET_LIFE: Duration = Duration::from_secs(10);

/// The longest a satellite waits for its hub to answer a call. The longest
/// call a client makes through a satellite, such as a history page, takes
/// a fraction of this on a hub, and the satellite must say the hub is away
/// before its own clients give up on it: `ritornelctl` gives up on finding
/// and greeting a server after ten seconds.
pub const HUB_CALL_WAIT: Duration = Duration::from_secs(8);

/// The bounds one server runs with. [`Default`] is what a server uses. A
/// test makes them small, so that it reaches one without opening a hundred
/// connections or waiting ten seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// See [`MAX_CONNECTIONS`].
    pub max_connections: usize,
    /// See [`MAX_SESSIONS`].
    pub max_sessions: usize,
    /// See [`HEADER_WAIT`].
    pub header_wait: Duration,
    /// See [`STOP_GRACE`].
    pub stop_grace: Duration,
    /// See [`TICKET_LIFE`].
    pub ticket_life: Duration,
    /// See [`HUB_CALL_WAIT`].
    pub hub_call_wait: Duration,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            max_sessions: MAX_SESSIONS,
            header_wait: HEADER_WAIT,
            stop_grace: STOP_GRACE,
            ticket_life: TICKET_LIFE,
            hub_call_wait: HUB_CALL_WAIT,
        }
    }
}
