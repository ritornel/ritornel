//! The listener: the HTTP routes and the socket, on loopback. See "Session
//! and frames" and "Media" in docs/internals/contract.md; "Listener and
//! transport boundaries" in docs/internals/linking.md; and "Process
//! lifecycle" in docs/internals/server.md.
//!
//! ```text
//! GET  /v1/descriptor            the public descriptor. The one route with no credential
//! GET  /v1/health                whether the server is up
//! POST /v1/ticket                trades the credential for a ticket that opens one socket
//! GET  /v1/ws                    the socket that carries calls and events
//! GET  /v1/art/{artId}/{size}    a cover, with the credential or a media token
//! POST /v1/link                  a satellite joining, with an invite's secret
//! GET  /v1/audio/{appearanceId}/{sourceItemId}/{revision}
//!                                a master, for a satellite, with its link session
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

mod cache;

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
use ritornel_contract::{
    ART_SIZES, Id, MEDIA_TOKEN_PARAM, NONCE_PARAM, Permission, PermissionSet, TICKET_PARAM, hex,
};
use ritornel_engine::SessionToken;
use ritornel_engine::playback::FileFacts;
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};

use crate::dispatch::{Core, MAX_MESSAGE_BYTES};
use crate::link::Peer;
use crate::session::SessionGrant;
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

/// The legacy webview `Origin` allowed on `/v1/ws` when paired with a
/// ticket. Keeping this narrow allowlist blocks browser pages while still
/// requiring a one-time ticket ("Query-string credentials and origin
/// checks" in docs/internals/contract.md). Native clients send no `Origin`.
const DESKTOP_ORIGIN: &str = "tauri://localhost";

/// The shortest and the longest nonce the descriptor signs, in bytes.
const MIN_NONCE_BYTES: usize = 16;
const MAX_NONCE_BYTES: usize = 64;

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
            // Windows refuses a port in a range the system has reserved,
            // as Hyper-V and WSL do, as a permission it lacks, not as taken.
            Some(fallback)
                if matches!(
                    taken.kind(),
                    ErrorKind::AddrInUse | ErrorKind::PermissionDenied
                ) =>
            {
                info!(%address, error = %taken, "the default port cannot be had: asking the system for another");
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
        listen: Listen,
        bounds: Bounds,
    ) -> Result<Self, Error> {
        let endpoint = listener.local_addr().map_err(Error::Runtime)?;
        let (stopping, stopped) = watch::channel(false);
        let tls = crate::tls::acceptor(core.signer())
            .map_err(|error| Error::Runtime(std::io::Error::other(error)))?;
        let router = router(core, Arc::new(tickets));
        let task = tokio::spawn(accept(listener, listen, router, tls, stopped, bounds));
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

    /// Where a client on this machine reaches it: the loopback address,
    /// on the port really bound, whatever interfaces it is bound to.
    pub(crate) fn local_endpoint(&self) -> SocketAddr {
        (std::net::Ipv4Addr::LOCALHOST, self.endpoint.port()).into()
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
    listen: Listen,
    router: Router,
    tls: TlsAcceptor,
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
            // Closed at once, before a byte is read: a peer from outside
            // the networks this server listens to is told nothing.
            Ok((_, peer)) if !listen.admits(peer.ip()) => {
                debug!(%peer, "a connection from outside the private network is closed");
            }
            Ok((stream, peer)) => {
                // From now, not from whenever the task first runs.
                let deadline = tokio::time::Instant::now() + bounds.header_wait;
                connections.spawn(serve(
                    stream,
                    peer,
                    router.clone(),
                    tls.clone(),
                    stopping.clone(),
                    (deadline, bounds.header_wait),
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

/// Whether a connection is TLS: what [`require_credential`] asks before
/// it takes a link session's token or a satellite joining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Secure(bool);

/// Serves one connection until it ends, holding its place among the
/// admitted connections until then.
///
/// Its first byte says what it is: a TLS handshake, which a satellite
/// opens with to reach its hub, or plain HTTP, which this machine's
/// clients and the private network's speak. The connection has until
/// `deadline`, `header_wait` from being accepted, to send a whole first
/// request, its handshake included, and Hyper's own `header_wait` for each
/// request after that.
async fn serve(
    stream: TcpStream,
    peer: SocketAddr,
    router: Router,
    tls: TlsAcceptor,
    mut stopping: watch::Receiver<bool>,
    (deadline, header_wait): (tokio::time::Instant, Duration),
    _admitted: OwnedSemaphorePermit,
) {
    // Whether the first request has reached the routes, after which the
    // deadline no longer applies.
    let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opened = async {
        let mut first = [0; 1];
        match stream.peek(&mut first).await {
            // Closed before it said anything.
            Ok(0) | Err(_) => None,
            Ok(_) if first[0] == crate::tls::HANDSHAKE => match tls.accept(stream).await {
                Ok(session) => Some(Opened::Tls(Box::new(session))),
                Err(failed) => {
                    debug!(%peer, error = %failed, "a TLS handshake failed");
                    None
                }
            },
            Ok(_) => Some(Opened::Plain(stream)),
        }
    };
    let opened = tokio::select! {
        () = stopped(&mut stopping) => return,
        opened = tokio::time::timeout_at(deadline, opened) => opened,
    };
    let Ok(Some(opened)) = opened else { return };
    match opened {
        Opened::Tls(session) => {
            serve_http(
                *session,
                Secure(true),
                router,
                asked,
                deadline,
                stopping,
                header_wait,
            )
            .await;
        }
        Opened::Plain(stream) => {
            serve_http(
                stream,
                Secure(false),
                router,
                asked,
                deadline,
                stopping,
                header_wait,
            )
            .await;
        }
    }
}

/// A connection, once its first byte has said what it is.
enum Opened {
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
    Plain(TcpStream),
}

/// Serves HTTP on a connection until it ends. Until its first request
/// reaches the routes, it is cut off at `deadline`.
async fn serve_http<S>(
    stream: S,
    secure: Secure,
    router: Router,
    asked: Arc<std::sync::atomic::AtomicBool>,
    deadline: tokio::time::Instant,
    mut stopping: watch::Receiver<bool>,
    header_wait: Duration,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use std::sync::atomic::Ordering;

    let seen = Arc::clone(&asked);
    let router = router
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            seen.store(true, Ordering::Relaxed);
            next.run(request)
        }))
        .layer(Extension(secure));
    let mut http = http1::Builder::new();
    // The deadline needs the timer: without one it would never fire.
    http.timer(TokioTimer::new())
        .header_read_timeout(header_wait)
        .max_buf_size(MAX_HEAD_BYTES);
    let connection = http
        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(router))
        .with_upgrades();
    tokio::pin!(connection);
    let mut waiting_for_first = true;
    // A connection that fails has failed for its own client alone, and
    // there is nobody to tell.
    loop {
        tokio::select! {
            _ = connection.as_mut() => return,
            () = stopped(&mut stopping) => {
                connection.as_mut().graceful_shutdown();
                let _ = connection.await;
                return;
            }
            () = tokio::time::sleep_until(deadline), if waiting_for_first => {
                if !asked.load(Ordering::Relaxed) {
                    return;
                }
                waiting_for_first = false;
            }
        }
    }
}

fn router(core: Arc<Core>, tickets: Arc<Tickets>) -> Router {
    Router::new()
        .route(route::DESCRIPTOR, get(descriptor))
        .route(route::HEALTH, get(health))
        .route(route::TICKET, post(ticket))
        .route(route::LINK, post(join))
        .route(route::WS, get(socket))
        .route(
            &format!("{}/{{key}}", route::CACHE_DERIVATIVE),
            get(cache::cached).layer(Extension(
                ritornel_contract::transcode::ArtifactLayer::Derivative,
            )),
        )
        .route(
            &format!("{}/{{key}}", route::CACHE_PACKAGED),
            get(cache::cached).layer(Extension(
                ritornel_contract::transcode::ArtifactLayer::Packaged,
            )),
        )
        .route(&format!("{}/{{art}}/{{size}}", route::ART), get(art))
        .route(
            &format!("{}/{{appearance}}/{{item}}/{{revision}}", route::AUDIO),
            get(audio),
        )
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
    let secure = request
        .extensions()
        .get::<Secure>()
        .is_some_and(|secure| secure.0);
    // The descriptor is public.
    if request.method() == Method::GET && request.uri().path() == route::DESCRIPTOR {
        return next.run(request).await;
    }
    // A satellite joining presents the invite's secret in its body, which
    // is checked there, and only over TLS: the secret is never read from a
    // connection anyone between could read too.
    if request.method() == Method::POST && request.uri().path() == route::LINK {
        if !secure {
            info!("a satellite tried to join without TLS: it runs an older release");
            return (
                StatusCode::FORBIDDEN,
                "join over TLS: this server runs a newer release than the one joining\n",
            )
                .into_response();
        }
        return next.run(request).await;
    }
    // Each check is a comparison in memory. It reads no file and no
    // database, so a request costs an async worker nothing it could wait on.
    let by_header = bearer(request.headers())
        .and_then(|token| core.authenticate(&token))
        // A link session's token is taken over TLS alone, which is all a
        // satellite speaks. Over plain HTTP it is refused like any unknown
        // token, and the satellite that sent it runs an older release.
        .filter(|(_, peer)| {
            let taken = secure || peer.is_none();
            if !taken {
                info!(satellite = ?peer.as_ref().map(|peer| peer.server), "a satellite's link session came without TLS: it runs an older release");
            }
            taken
        })
        .map(|(grant, peer)| (grant, peer, Presented::Header));
    let admitted = by_header
        .or_else(|| {
            socket_ticket(&request)
                .and_then(|ticket| tickets.redeem(ticket))
                .map(|grant| (grant, None, Presented::Ticket))
        })
        .or_else(|| {
            // A media token reads artwork and nothing else: it cannot open
            // a WebSocket session or exchange for a ticket.
            media_token(&request)
                .filter(|token| core.media_accepts(token))
                .map(|_| {
                    (
                        SessionGrant::custom(
                            None,
                            PermissionSet::from_slice(&[Permission::LibraryRead]),
                        ),
                        None,
                        Presented::MediaToken,
                    )
                })
        });
    match admitted {
        Some((grant, peer, presented)) => {
            request.extensions_mut().insert(grant);
            request.extensions_mut().insert(peer);
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

/// The public descriptor. With `?nonce=` it carries the server's proof
/// that it holds its key, signed for that nonce.
async fn descriptor(State(core): State<Arc<Core>>, request: Request) -> Response {
    let nonce = request.uri().query().and_then(|query| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(NONCE_PARAM)?.strip_prefix('='))
    });
    match nonce.map(hex::decode) {
        None => json(&core.public_descriptor(None)),
        Some(Some(nonce)) if (MIN_NONCE_BYTES..=MAX_NONCE_BYTES).contains(&nonce.len()) => {
            json(&core.public_descriptor(Some(&nonce)))
        }
        Some(_) => (StatusCode::BAD_REQUEST, "a nonce is 32 to 128 hex digits\n").into_response(),
    }
}

async fn health(Extension(grant): Extension<SessionGrant>) -> Response {
    if !grant.permissions.contains(Permission::StatusRead) {
        return (StatusCode::FORBIDDEN, "health requires status:read\n").into_response();
    }
    json(&Health {
        status: HealthStatus::Ok,
    })
}

/// Trades the credential for a ticket that opens one socket carrying the
/// permissions the credential holds.
async fn ticket(
    Extension(tickets): Extension<Arc<Tickets>>,
    Extension(grant): Extension<SessionGrant>,
    Extension(peer): Extension<Option<Peer>>,
    headers: HeaderMap,
) -> Response {
    // A satellite opens its socket with its link session in the header,
    // and a ticket would lose which satellite it is.
    if peer.is_some() {
        return (
            StatusCode::FORBIDDEN,
            "a link session opens its socket itself\n",
        )
            .into_response();
    }
    // A program asks for a ticket, and a program names no origin. A page
    // does, and no page is given one: its shell asks for it. A browser
    // would not get this far in any case, since a request with an
    // `Authorization` header is preflighted and no preflight is answered.
    if headers.contains_key(header::ORIGIN) {
        return (StatusCode::FORBIDDEN, "a page may not ask for a ticket\n").into_response();
    }
    match tickets.issue(grant) {
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

/// How much of a master is read from the disk at a time while it is sent.
const AUDIO_CHUNK: usize = 256 * 1024;

/// A master, for a satellite to play: `GET /v1/audio/{appearanceId}/
/// {sourceItemId}/{revision}`, as `link.copies` named it. Only a link
/// session reads it. The file is sent as it is read, and refused if it is
/// no longer what the scan read at that revision: its size or its time has
/// changed, or a later scan has read it again.
async fn audio(
    State(core): State<Arc<Core>>,
    Extension(grant): Extension<SessionGrant>,
    Extension(peer): Extension<Option<Peer>>,
    Path((appearance, item, revision)): Path<(String, String, String)>,
) -> Response {
    if peer.is_none() || !grant.permissions.contains(Permission::LibraryRead) {
        return (
            StatusCode::FORBIDDEN,
            "only a satellite with library:read reads masters\n",
        )
            .into_response();
    }
    let (Ok(appearance), Ok(item), Ok(revision)) = (
        appearance.parse::<Id>(),
        item.parse::<Id>(),
        revision.parse::<i64>(),
    ) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let pace = core.audio_pace();
    // Reading the library and opening the file wait on the disk.
    let opened = tokio::task::spawn_blocking(move || {
        let (path, scanned) = core.master(appearance, item, revision)?;
        let file = std::fs::File::open(path).ok()?;
        let now = FileFacts::of(&file.metadata().ok()?);
        Some((file, now, scanned))
    })
    .await;
    let Ok(Some((file, now, scanned))) = opened else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let size = now.size_bytes;
    if now != scanned {
        return (
            StatusCode::CONFLICT,
            "the file has changed since it was scanned\n",
        )
            .into_response();
    }
    let chunks = futures_util::stream::unfold(Some(file), move |file| async move {
        let mut file = file?;
        if let Some(pace) = pace {
            tokio::time::sleep(pace).await;
        }
        let read = tokio::task::spawn_blocking(move || {
            let mut chunk = vec![0; AUDIO_CHUNK];
            let read = std::io::Read::read(&mut file, &mut chunk)?;
            chunk.truncate(read);
            Ok::<_, std::io::Error>((file, chunk))
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(|read| read);
        match read {
            Ok((_, chunk)) if chunk.is_empty() => None,
            Ok((file, chunk)) => Some((Ok(axum::body::Bytes::from(chunk)), Some(file))),
            Err(failed) => Some((Err(failed), None)),
        }
    });
    (
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (header::CONTENT_LENGTH, size.to_string()),
        ],
        axum::body::Body::from_stream(chunks),
    )
        .into_response()
}

/// The largest cover a satellite takes from its hub.
const MAX_COVER_BYTES: usize = 16 << 20;

/// The largest body a satellite may send to join.
const MAX_LINK_BODY: usize = 4 * 1024;

/// How long a satellite joining has to send its body.
const LINK_BODY_WAIT: Duration = Duration::from_secs(5);

/// A satellite joining: `POST /v1/link`, with the invite's secret in a
/// [`LinkRequest`](ritornel_contract::LinkRequest). It is answered with the
/// link session's token, or refused with why.
async fn join(State(core): State<Arc<Core>>, request: Request) -> Response {
    // A server names no origin, and a page in a browser is not let try.
    if request.headers().contains_key(header::ORIGIN) {
        return (StatusCode::FORBIDDEN, "a page may not link a server\n").into_response();
    }
    // The route takes no credential, so a body that never ends must not
    // hold a connection's place for long.
    let body = tokio::time::timeout(
        LINK_BODY_WAIT,
        axum::body::to_bytes(request.into_body(), MAX_LINK_BODY),
    )
    .await;
    let Ok(Ok(body)) = body else {
        return (
            StatusCode::BAD_REQUEST,
            "the request was too long or too slow\n",
        )
            .into_response();
    };
    let Ok(asked) = serde_json::from_slice::<ritornel_contract::LinkRequest>(&body) else {
        return (StatusCode::BAD_REQUEST, "that is not a request to link\n").into_response();
    };
    // It writes both databases.
    let granted = tokio::task::spawn_blocking(move || core.grant_link(&asked)).await;
    match granted {
        Ok(Ok(granted)) => {
            let mut response = json(&granted);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Ok(Err((status, why))) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            why,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// A cover: `GET /v1/art/{artId}/{size}`. From the cache if it is there,
/// or else made on a cover thread, newest request first. A client that
/// goes before its cover is made has it dropped from the queue.
///
/// The answer never changes for one URL, so it may be kept for a year.
/// A cover that cannot be found, at a size that is not a bucket or for an
/// art ID that is not one, is 404.
async fn art(
    State(core): State<Arc<Core>>,
    Extension(grant): Extension<SessionGrant>,
    Path((art, size)): Path<(String, String)>,
) -> Response {
    if !grant.permissions.contains(Permission::LibraryRead) {
        return (StatusCode::FORBIDDEN, "artwork requires library:read\n").into_response();
    }
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
    let read = cached.clone();
    if let Ok(Ok(bytes)) = tokio::task::spawn_blocking(move || std::fs::read(read)).await {
        return jpeg(bytes);
    }
    // A satellite has no pictures: it asks its hub, over TLS pinned to the
    // hub's key on the route where the hub last answered, and keeps what it
    // is given beside the covers it would have made.
    if let Some(uplink) = core.uplink() {
        let Some(route) = uplink.route() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let path = format!("{}/{art}/{size}", route::ART);
        let credential = uplink.credential();
        // Over TLS pinned to the hub's key, as everything of the link is.
        let fetched = match uplink.dial(route).await {
            Ok(stream) => ritornel_client::get_media_over(
                stream,
                route,
                &path,
                Some(&credential),
                MAX_COVER_BYTES,
            )
            .await
            .map_err(|error| error.to_string()),
            Err(why) => Err(why),
        };
        return match fetched {
            Ok(answer) if answer.status == 200 => {
                let body = answer.body.clone();
                // Written beside it and moved into place, so a reader never
                // finds half a cover.
                let kept = tokio::task::spawn_blocking(move || {
                    if let Some(folder) = cached.parent() {
                        std::fs::create_dir_all(folder)?;
                    }
                    let part = cached.with_extension("part");
                    std::fs::write(&part, body)?;
                    std::fs::rename(&part, &cached).inspect_err(|_| {
                        let _ = std::fs::remove_file(&part);
                    })
                })
                .await;
                if !matches!(kept, Ok(Ok(()))) {
                    debug!("a cover from the hub could not be kept");
                }
                jpeg(answer.body)
            }
            Ok(answer) if answer.status == 404 => StatusCode::NOT_FOUND.into_response(),
            _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
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
    Extension(grant): Extension<SessionGrant>,
    Extension(peer): Extension<Option<Peer>>,
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
            if let Err(mut socket) = core.run(grant, peer, socket) {
                // The server began to stop, or the last place was taken,
                // while the socket was being opened.
                let _ = socket.send(Message::Close(None)).await;
            }
        })
}

/// The socket as a transport: one text message is one frame.
impl Wire for WebSocket {
    async fn recv(&mut self) -> Option<crate::transport::Incoming> {
        use crate::transport::Incoming;
        match WebSocket::recv(self).await? {
            Ok(Message::Text(text)) => Some(Incoming::Text(text.as_str().to_owned())),
            // The socket layer answers a ping by itself.
            Ok(Message::Ping(_) | Message::Pong(_)) => Some(Incoming::Alive),
            // A frame is text. Bytes are not this contract, and neither
            // is a message over the limit, which arrives as an error
            // before it has been buffered.
            Ok(Message::Binary(_) | Message::Close(_)) | Err(_) => None,
        }
    }

    async fn ping(&mut self) -> bool {
        WebSocket::send(self, Message::Ping(Default::default()))
            .await
            .is_ok()
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
