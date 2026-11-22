//! The listener: the HTTP routes and the socket, on loopback. See "Session
//! and frames" and "Media" in docs/design/contract.md, decisions 5 and 8 in
//! docs/design/linking.md, and "Process lifecycle" in
//! docs/internals/server.md.
//!
//! ```text
//! GET  /v1/descriptor            the public descriptor. The one route with no credential
//! GET  /v1/health                whether the server is up
//! POST /v1/ticket                trades the credential for a ticket that opens one socket
//! GET  /v1/ws                    the socket that carries calls and events
//! GET  /v1/art/{artId}/{size}    a cover, with the credential or a media token
//! ```
//!
//! A connection is one of at most `Bounds::max_connections` being served,
//! and has `Bounds::header_wait` to send a request, so connections that
//! say nothing cost a bounded number of file descriptors for a bounded
//! time. That holds before any credential is checked.
//!
//! Every route but the descriptor needs a credential, loopback included,
//! because any web page can send a request to `127.0.0.1`. The one
//! credential there is yet is the local one, and it rides in an
//! `Authorization: Bearer` header, which a native client can send and a
//! page opening a socket cannot.
//!
//! The desktop's page therefore opens the socket with a ticket in the
//! query string, which its shell got for it by presenting the credential
//! (`ticket`). That is the only thing a page is let do here. The socket
//! takes one origin, the desktop page's, and only with a ticket; the
//! ticket route takes no origin at all; and no route answers a preflight,
//! so a page in a browser can neither get a ticket nor use one.
//!
//! The socket is a thin codec. A text message in is one frame for the
//! dispatcher, and each frame queued on the session goes out as one text
//! message.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use ritornel_contract::route::{self, Health, HealthStatus};
use ritornel_contract::{ART_SIZES, Id, MEDIA_TOKEN_PARAM, SessionKind, TICKET_PARAM};
use ritornel_engine::SessionToken;
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{error, info, warn};

use crate::dispatch::{Core, MAX_MESSAGE_BYTES};
use crate::ticket::Tickets;
use crate::transport::{self, Wire};
use crate::{Bounds, Error, Listen};

/// How long the listener waits before it tries again after the system
/// refused to hand over a connection, which it does when the process is out
/// of file descriptors. Without it the loop would spin.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

/// The most a connection may send as the head of a request. The requests
/// of this contract are a line, a credential and a few headers.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// The `Origin` the desktop app's page presents on macOS. Every Tauri app
/// presents it, so it keeps web pages out and does not say the page is
/// Ritornel's: the ticket does that (docs/design/contract.md, "Origin
/// check"). This string is all the server knows of the desktop's shell.
const DESKTOP_ORIGIN: &str = "tauri://localhost";

/// Where a request carried what let it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// The credential, in an `Authorization` header.
    Header,
    /// A ticket, in the query string of the socket's route.
    Ticket,
    /// A media token, in the query string of a media route.
    MediaToken,
}

/// Binds the listener. Nobody having chosen a port, a taken default port
/// means a port the system picks.
pub(crate) async fn bind(listen: Listen) -> Result<TcpListener, Error> {
    let address = listen.address();
    match TcpListener::bind(address).await {
        Ok(listener) => Ok(listener),
        Err(taken) => match listen.fallback() {
            Some(fallback) if taken.kind() == ErrorKind::AddrInUse => {
                info!(%address, "the default port is taken: asking the system for another");
                TcpListener::bind(fallback)
                    .await
                    .map_err(|source| Error::Listen {
                        address: fallback,
                        source,
                    })
            }
            _ => Err(Error::Listen {
                address,
                source: taken,
            }),
        },
    }
}

/// A listener that is serving.
#[derive(Debug)]
pub(crate) struct Listening {
    endpoint: SocketAddr,
    stopping: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Listening {
    /// Starts serving `core` on a bound listener.
    pub(crate) fn start(
        core: Arc<Core>,
        tickets: Tickets,
        listener: TcpListener,
        bounds: Bounds,
    ) -> Result<Self, Error> {
        let endpoint = listener.local_addr().map_err(Error::Runtime)?;
        let (stopping, stopped) = watch::channel(false);
        let router = router(core, Arc::new(tickets));
        let task = tokio::spawn(accept(listener, router, stopped, bounds));
        Ok(Self {
            endpoint,
            stopping,
            task,
        })
    }

    /// The address and the port really bound.
    pub(crate) fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }

    /// Stops accepting connections and ends the ones there are: each gets
    /// the stop grace to finish the request it is in, and is then cut off.
    /// When this returns no connection is being served. A socket is not a
    /// connection any more: closing its session is what ends it.
    pub(crate) async fn stop(mut self) {
        self.stopping.send_replace(true);
        // The task ends by itself, within the grace it gives its
        // connections. It has failed only if it panicked.
        let _ = (&mut self.task).await;
    }
}

impl Drop for Listening {
    fn drop(&mut self) {
        // After `stop` the task has ended and this does nothing. Its
        // connections end with it: they are its own.
        self.task.abort();
    }
}

/// Resolves when the listener has been told to stop, or has been dropped.
async fn stopped(stopping: &mut watch::Receiver<bool>) {
    let _ = stopping.wait_for(|stopping| *stopping).await;
}

/// Accepts connections until told to stop, never serving more than
/// `bounds.max_connections` at once. With that many being served the next
/// one waits, in the system's own queue, for one of them to end, and each
/// of them ends: a request is answered at once, and a connection that does
/// not send one is closed after `bounds.header_wait`.
async fn accept(
    listener: TcpListener,
    router: Router,
    mut stopping: watch::Receiver<bool>,
    bounds: Bounds,
) {
    let admitted = Arc::new(Semaphore::new(bounds.max_connections));
    let mut connections = JoinSet::new();
    loop {
        // Connections that have ended are forgotten as the loop goes round.
        while connections.try_join_next().is_some() {}

        let admitted = tokio::select! {
            () = stopped(&mut stopping) => break,
            admitted = Arc::clone(&admitted).acquire_owned() => admitted,
        };
        // The semaphore is never closed, so there is always a permit to
        // wait for.
        let Ok(admitted) = admitted else { break };
        let accepted = tokio::select! {
            () = stopped(&mut stopping) => break,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, _)) => {
                connections.spawn(serve(
                    stream,
                    router.clone(),
                    stopping.clone(),
                    bounds.header_wait,
                    admitted,
                ));
            }
            Err(failed) => {
                warn!(error = %failed, "a connection could not be accepted");
                tokio::time::sleep(ACCEPT_PAUSE).await;
            }
        }
    }
    drop(listener);

    // Each connection has seen the stop and is finishing its request.
    if !transport::end_within(&mut connections, bounds.stop_grace).await {
        warn!("requests were still in flight: their connections were cut off");
    }
}

/// Serves one connection until it ends, holding its place among the
/// admitted connections until then.
async fn serve(
    stream: TcpStream,
    router: Router,
    mut stopping: watch::Receiver<bool>,
    header_wait: Duration,
    _admitted: OwnedSemaphorePermit,
) {
    let mut http = http1::Builder::new();
    // The deadline needs the timer: without one it would never fire.
    http.timer(TokioTimer::new())
        .header_read_timeout(header_wait)
        .max_buf_size(MAX_HEAD_BYTES);
    let connection = http
        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(router))
        .with_upgrades();
    tokio::pin!(connection);
    // A connection that fails has failed for its own client alone, and
    // there is nobody to tell.
    tokio::select! {
        _ = connection.as_mut() => {}
        () = stopped(&mut stopping) => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        }
    }
}

fn router(core: Arc<Core>, tickets: Arc<Tickets>) -> Router {
    Router::new()
        .route(route::DESCRIPTOR, get(descriptor))
        .route(route::HEALTH, get(health))
        .route(route::TICKET, post(ticket))
        .route(route::WS, get(socket))
        .route(&format!("{}/{{art}}/{{size}}", route::ART), get(art))
        // A path the server does not have is checked like any other, so
        // asking without a credential shows nothing of what exists.
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn_with_state(
            Arc::clone(&core),
            require_credential,
        ))
        // Outside the check, so that the check can see it.
        .layer(Extension(tickets))
        .with_state(core)
}

/// Lets a request through if it is for the public descriptor, carries a
/// credential the server issued, or is opening the socket with a ticket
/// the server issued. It says what kind of session that holds, and which
/// of the two it was.
///
/// The places a credential may ride are tried in order: the header, then
/// the query string. A ticket that is tried is spent, whatever becomes of
/// the request.
async fn require_credential(
    State(core): State<Arc<Core>>,
    Extension(tickets): Extension<Arc<Tickets>>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.method() == Method::GET && request.uri().path() == route::DESCRIPTOR {
        return next.run(request).await;
    }
    // Each check is a comparison in memory. It reads no file and no
    // database, so a request costs an async worker nothing it could wait on.
    let by_header = bearer(request.headers())
        .and_then(|token| core.authenticate(&token))
        .map(|kind| (kind, Presented::Header));
    let admitted = by_header
        .or_else(|| {
            socket_ticket(&request)
                .and_then(|ticket| tickets.redeem(ticket))
                .map(|kind| (kind, Presented::Ticket))
        })
        .or_else(|| {
            // A media token reads media, and nothing else: a session of no
            // kind is opened with it.
            media_token(&request)
                .filter(|token| core.media_accepts(token))
                .map(|_| (SessionKind::Limited, Presented::MediaToken))
        });
    match admitted {
        Some((kind, presented)) => {
            request.extensions_mut().insert(kind);
            request.extensions_mut().insert(presented);
            next.run(request).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "a credential is needed\n",
        )
            .into_response(),
    }
}

/// The token of an `Authorization: Bearer` header. It is wrapped at once,
/// so it cannot be printed.
fn bearer(headers: &HeaderMap) -> Option<SessionToken> {
    let (scheme, token) = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| SessionToken::new(token.trim()))
}

/// The ticket in the query string of a request to open the socket. No
/// other route takes one. It is handed on as it came and never printed.
fn socket_ticket(request: &Request) -> Option<&str> {
    if request.method() != Method::GET || request.uri().path() != route::WS {
        return None;
    }
    request
        .uri()
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix(TICKET_PARAM)?.strip_prefix('='))
}

/// The media token in the query string of a request for media. No other
/// route takes one. It is handed on as it came and never printed.
fn media_token(request: &Request) -> Option<&str> {
    let path = request.uri().path();
    if request.method() != Method::GET
        || !path
            .strip_prefix(route::ART)
            .is_some_and(|rest| rest.starts_with('/'))
    {
        return None;
    }
    request
        .uri()
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix(MEDIA_TOKEN_PARAM)?.strip_prefix('='))
}

fn json<T: Serialize>(body: &T) -> Response {
    match serde_json::to_string(body) {
        Ok(text) => ([(header::CONTENT_TYPE, "application/json")], text).into_response(),
        Err(failed) => {
            error!(error = %failed, "a response could not be written");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn descriptor(State(core): State<Arc<Core>>) -> Response {
    json(&core.public_descriptor())
}

async fn health() -> Response {
    json(&Health {
        status: HealthStatus::Ok,
    })
}

/// Trades the credential for a ticket that opens one socket of the kind
/// the credential holds.
async fn ticket(
    Extension(tickets): Extension<Arc<Tickets>>,
    Extension(kind): Extension<SessionKind>,
    headers: HeaderMap,
) -> Response {
    // A program asks for a ticket, and a program names no origin. A page
    // does, and no page is given one: its shell asks for it. A browser
    // would not get this far in any case, since a request with an
    // `Authorization` header is preflighted and no preflight is answered.
    if headers.contains_key(header::ORIGIN) {
        return (StatusCode::FORBIDDEN, "a page may not ask for a ticket\n").into_response();
    }
    match tickets.issue(kind) {
        Ok(issued) => {
            let mut response = json(&issued);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(failed) => {
            error!(error = %failed, "a ticket could not be made");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// A cover: `GET /v1/art/{artId}/{size}`. From the cache if it is there,
/// or else made on a cover thread, newest request first. A client that
/// goes before its cover is made has it dropped from the queue.
///
/// The answer never changes for one URL, so it may be kept for a year.
/// A cover that cannot be found, at a size that is not a bucket or for an
/// art ID that is not one, is 404.
async fn art(State(core): State<Arc<Core>>, Path((art, size)): Path<(String, String)>) -> Response {
    let (Ok(art), Ok(size)) = (art.parse::<Id>(), size.parse::<u32>()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !ART_SIZES.contains(&size) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let jpeg = |bytes: Vec<u8>| {
        (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response()
    };
    // Reading a file waits on the disk, so it is not done on an async
    // worker either.
    let cached = core.cover_path(art, size);
    if let Ok(Ok(bytes)) = tokio::task::spawn_blocking(move || std::fs::read(cached)).await {
        return jpeg(bytes);
    }
    let (send, made) = tokio::sync::oneshot::channel();
    let worker = Arc::clone(&core);
    // Dropped with this request, which drops the work if it has not begun.
    let _wanted = core.submit_cover(move || {
        let _ = send.send(worker.render_cover(art, size));
    });
    match made.await {
        Ok(Ok(rendered)) => jpeg(rendered.bytes),
        Ok(Err(
            ritornel_engine::covers::CoverError::NotFound
            | ritornel_engine::covers::CoverError::BadSize,
        )) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        // The server is stopping, or making the cover panicked.
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Whether a request that names these origins may open the socket.
///
/// A browser names the page that opened the socket, and a native client
/// names nothing. The one page the server knows is the desktop's, and it is
/// known only when it comes with a ticket: a page cannot send the header,
/// so a request that has both an origin and the header is not the
/// desktop's page.
fn origin_may_open_a_socket(headers: &HeaderMap, presented: Presented) -> bool {
    let mut origins = headers.get_all(header::ORIGIN).iter().peekable();
    origins.peek().is_none()
        || (presented == Presented::Ticket
            && origins.all(|origin| origin.as_bytes() == DESKTOP_ORIGIN.as_bytes()))
}

async fn socket(
    State(core): State<Arc<Core>>,
    Extension(kind): Extension<SessionKind>,
    Extension(presented): Extension<Presented>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !origin_may_open_a_socket(&headers, presented) {
        return (StatusCode::FORBIDDEN, "this origin may not open a socket\n").into_response();
    }
    // Refused here, before the upgrade, so the client is told why. A
    // session that takes the last place between this and the upgrade is
    // found out when the socket opens.
    if !core.has_room() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "this server has as many sessions open as it takes\n",
        )
            .into_response();
    }
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            // The session runs as a task the server owns, not as this one.
            if let Err(mut socket) = core.run(kind, socket) {
                // The server began to stop, or the last place was taken,
                // while the socket was being opened.
                let _ = socket.send(Message::Close(None)).await;
            }
        })
}

/// The socket as a transport: one text message is one frame.
impl Wire for WebSocket {
    async fn recv(&mut self) -> Option<String> {
        loop {
            match WebSocket::recv(self).await? {
                Ok(Message::Text(text)) => return Some(text.as_str().to_owned()),
                // The socket layer answers a ping by itself.
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                // A frame is text. Bytes are not this contract, and neither
                // is a message over the limit, which arrives as an error
                // before it has been buffered.
                Ok(Message::Binary(_) | Message::Close(_)) | Err(_) => return None,
            }
        }
    }

    async fn send(&mut self, text: String) -> bool {
        WebSocket::send(self, Message::Text(text.into()))
            .await
            .is_ok()
    }

    async fn close(&mut self) {
        // The client may have gone already.
        let _ = WebSocket::send(self, Message::Close(None)).await;
    }
}
