//! A connection to the server, once one is found (`link.rs` finds it, and
//! finds it again). Calls and events go over `ritornel_client` on one Tokio
//! runtime for the app's life; GPUI's foreground awaits Tokio's join handles
//! directly, since they wake on any executor.
//!
//! Covers come over HTTP (`GET /v1/art/{artId}/{size}?token=`), on a few
//! kept-alive connections. `ritornel_client::get` reads at most 64 KB of a
//! body, which a large cover passes, so this has its own small GET.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use hyper::client::conn::http1::SendRequest;
use hyper::header::HOST;
use hyper_util::rt::TokioIo;
use ritornel_client::Client;
use ritornel_client::local::LocalServer;
use ritornel_contract::{
    CONTRACT_VERSION, ClientKind, ErrorCode, Event, HelloParams, Id, MEDIA_TOKEN_PARAM, ServerRole,
    ServerStatus, Topic, Versions, route,
};
use tokio::net::TcpStream;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedReceiver;

/// Covers asked of the server, for the script's `count` step.
static ART_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many covers have been asked of the server so far.
pub(crate) fn art_reads() -> u64 {
    ART_READS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The runtime calls, events and covers run on, for as long as the app does.
pub(crate) fn rt() -> &'static Handle {
    static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();
    static FALLBACK: OnceLock<Runtime> = OnceLock::new();
    let built = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("net")
            .enable_all()
            .build()
            .ok()
    });
    match built {
        Some(runtime) => runtime.handle(),
        // A runtime that cannot be built with two threads is built with the
        // current one: the app works, a little less smoothly.
        None => FALLBACK
            .get_or_init(|| {
                #[allow(clippy::expect_used)]
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a Tokio runtime")
            })
            .handle(),
    }
}

/// One session with the server.
pub(crate) struct Net {
    pub client: Arc<Client>,
    pub art: Arc<ArtHttp>,
    pub art_sizes: Vec<u32>,
    /// The server's name, as the title bar shows it.
    pub name: String,
    pub role: ServerRole,
    pub versions: Option<Versions>,
    /// Whether the server holds a library.
    pub holds: bool,
    /// On a satellite, its hub, and whether it answers.
    pub hub: Option<ritornel_contract::HubStatus>,
}

impl Net {
    /// Opens a session with a server that has been found: says hello,
    /// subscribes to everything the app follows, and reads what the Library
    /// page needs to say. The events come back apart, to be read on a task
    /// of their own; their end is the session's.
    pub(crate) async fn open(
        server: &LocalServer,
    ) -> Result<(Self, UnboundedReceiver<Event>), ritornel_client::Error> {
        let mut client = server.connect().await?;
        let descriptor = client
            .hello(&HelloParams {
                name: "ritornel-desktop-gpui".to_owned(),
                kind: ClientKind::Desktop,
                version: env!("CARGO_PKG_VERSION").to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await?;
        let events = client.take_events();
        client
            .subscribe(&[
                Topic::Servers,
                // Scans and imports as they run, for Settings.
                Topic::Jobs,
                Topic::Library,
                Topic::List,
                Topic::Playback,
                Topic::Progress,
                Topic::Queue,
            ])
            .await?;
        let token = client.media_token().await?.token;
        // A session of the local owner is `full`, and may read the status.
        let status = client.status().await.ok();
        let art = Arc::new(ArtHttp {
            endpoint: server.endpoint,
            token,
            idle: Mutex::new(Vec::new()),
            in_flight: Semaphore::new(MAX_ART_IN_FLIGHT),
        });
        Ok((
            Self {
                client: Arc::new(client),
                art,
                art_sizes: descriptor.server.art_sizes,
                name: status
                    .as_ref()
                    .map_or(descriptor.server.name, |status| status.name.clone()),
                role: status
                    .as_ref()
                    .map_or(descriptor.server.role, |status| status.role),
                versions: status.as_ref().map(|status| status.versions.clone()),
                holds: status
                    .as_ref()
                    .is_none_or(|status| status.library.is_some()),
                hub: status.as_ref().and_then(|status| status.hub.clone()),
            },
            events,
        ))
    }

    /// The same session, with what the server now says of itself.
    pub(crate) fn restated(&self, status: &ServerStatus) -> Self {
        Self {
            client: Arc::clone(&self.client),
            art: Arc::clone(&self.art),
            art_sizes: self.art_sizes.clone(),
            name: status.name.clone(),
            role: status.role,
            versions: Some(status.versions.clone()),
            holds: status.library.is_some(),
            hub: status.hub.clone(),
        }
    }

    /// Whether `other` is the same server session, even after its status was
    /// read again (`restated`).
    pub(crate) fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.client, &other.client)
    }

    /// A session over `client` with `role`, for tests.
    #[cfg(test)]
    pub(crate) fn test_session(client: Arc<Client>, role: ServerRole) -> Self {
        Self {
            client,
            art: Arc::new(ArtHttp {
                endpoint: SocketAddr::from(([127, 0, 0, 1], 9)),
                token: String::new(),
                idle: Mutex::new(Vec::new()),
                in_flight: Semaphore::new(MAX_ART_IN_FLIGHT),
            }),
            art_sizes: vec![300],
            name: "Test Server".to_owned(),
            role,
            versions: None,
            holds: role != ServerRole::Unassigned,
            hub: None,
        }
    }

    /// A session over `client`, for tests: a satellite whose hub answers
    /// or not, with no covers to read.
    #[cfg(test)]
    pub(crate) fn satellite(client: Arc<Client>, answers: bool) -> Self {
        let id = ritornel_contract::Id::from_bytes([1; 16]);
        Self {
            client,
            art: Arc::new(ArtHttp {
                endpoint: SocketAddr::from(([127, 0, 0, 1], 9)),
                token: String::new(),
                idle: Mutex::new(Vec::new()),
                in_flight: Semaphore::new(MAX_ART_IN_FLIGHT),
            }),
            art_sizes: vec![300],
            name: "Study Mac".to_owned(),
            role: ritornel_contract::ServerRole::Satellite,
            versions: None,
            holds: true,
            hub: Some(ritornel_contract::HubStatus {
                server_id: id,
                name: "Studio Mac".to_owned(),
                library_id: id,
                epoch: 1,
                routes: Vec::new(),
                linked_at_ms: 0,
                answers: Some(answers),
                since_ms: Some(0),
                route: None,
                problem: None,
            }),
        }
    }

    /// The same, reading pictures from `endpoint` at `sizes`, for tests.
    #[cfg(test)]
    pub(crate) fn with_art(mut self, endpoint: SocketAddr, sizes: Vec<u32>) -> Self {
        self.art = Arc::new(ArtHttp::at(endpoint));
        self.art_sizes = sizes;
        self
    }

    pub(crate) fn rt(&self) -> &'static Handle {
        rt()
    }

    /// The bucket a cover drawn `px` pixels wide is asked for at.
    pub(crate) fn bucket(&self, px: u32) -> u32 {
        bucket_in(&self.art_sizes, px)
    }
}

/// Whether a call was refused because the server holds no library yet.
pub(crate) fn no_library(error: &ritornel_client::Error) -> bool {
    matches!(
        error,
        ritornel_client::Error::Call(rpc)
            if rpc.data.code == ErrorCode::Unavailable
                && rpc.data.details.get("reason").and_then(|reason| reason.as_str())
                    == Some("no_library")
    )
}

/// A few HTTP/1.1 connections to the server, kept alive between covers.
pub(crate) struct ArtHttp {
    endpoint: SocketAddr,
    token: String,
    idle: Mutex<Vec<SendRequest<Empty<Bytes>>>>,
    in_flight: Semaphore,
}

/// The largest cover read: 1024 pixels square is well under it.
const MAX_ART_BYTES: usize = 8 * 1024 * 1024;
/// Most cover fetches run at once, across the wall and any open view, so a
/// prolific artist or album page does not open dozens of sockets at once.
const MAX_ART_IN_FLIGHT: usize = 12;

/// Why a cover could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoArt {
    /// The server has none: asked again, it gives the same answer.
    Missing,
    /// It could not be read this time: the connection, or the server, failed.
    Failed,
}

impl ArtHttp {
    /// Covers read from `endpoint`, for tests.
    #[cfg(test)]
    pub(crate) fn at(endpoint: SocketAddr) -> Self {
        Self {
            endpoint,
            token: String::new(),
            idle: Mutex::new(Vec::new()),
            in_flight: Semaphore::new(MAX_ART_IN_FLIGHT),
        }
    }

    /// Reads one cover. Dropped part way, the connection is closed, and the
    /// server drops the cover from its queue if it has not begun it.
    pub(crate) async fn get(&self, art: Id, size: u32) -> Result<Vec<u8>, NoArt> {
        fn failed<E>(_: E) -> NoArt {
            NoArt::Failed
        }
        let _permit = self.in_flight.acquire().await.map_err(failed)?;
        ART_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let idle = self.idle.lock().ok().and_then(|mut idle| idle.pop());
        let mut sender = match idle {
            Some(sender) if !sender.is_closed() => sender,
            _ => {
                let stream = TcpStream::connect(self.endpoint).await.map_err(failed)?;
                stream.set_nodelay(true).map_err(failed)?;
                let (sender, connection) =
                    hyper::client::conn::http1::handshake(TokioIo::new(stream))
                        .await
                        .map_err(failed)?;
                tokio::spawn(connection);
                sender
            }
        };
        sender.ready().await.map_err(failed)?;
        let path = format!(
            "{}/{art}/{size}?{MEDIA_TOKEN_PARAM}={}",
            route::ART,
            self.token
        );
        let request = hyper::Request::get(path)
            .header(HOST, self.endpoint.to_string())
            .body(Empty::<Bytes>::new())
            .map_err(failed)?;
        let response = sender.send_request(request).await.map_err(failed)?;
        let status = response.status();
        let body = Limited::new(response.into_body(), MAX_ART_BYTES)
            .collect()
            .await
            .map_err(failed)?
            .to_bytes();
        // At most MAX_ART_IN_FLIGHT permits exist, so the idle pool stays bounded.
        if let Ok(mut idle) = self.idle.lock() {
            idle.push(sender);
        }
        if status.is_success() {
            Ok(body.to_vec())
        } else if status == hyper::StatusCode::NOT_FOUND {
            Err(NoArt::Missing)
        } else {
            Err(NoArt::Failed)
        }
    }
}

/// The smallest of the server's sizes `sizes` that is at least `px`, or its
/// largest.
pub(crate) fn bucket_in(sizes: &[u32], px: u32) -> u32 {
    sizes
        .iter()
        .copied()
        .find(|size| *size >= px)
        .or_else(|| sizes.last().copied())
        .unwrap_or(256)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[test]
    fn art_http_bounds_in_flight_requests_and_idle_connections()
    -> Result<(), Box<dyn std::error::Error>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let endpoint = listener.local_addr()?;
            let active = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let server_active = Arc::clone(&active);
            let server_peak = Arc::clone(&peak);
            let server = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let active = Arc::clone(&server_active);
                    let peak = Arc::clone(&server_peak);
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 512];
                        while let Ok(n) = socket.read(&mut chunk).await {
                            if n == 0 {
                                break;
                            }
                            if let Some(bytes) = chunk.get(..n) {
                                buf.extend_from_slice(bytes);
                            }
                            while let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                buf.drain(..end + 4);
                                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                                peak.fetch_max(now, Ordering::SeqCst);
                                // sleep: a slow reply, so that requests overlap
                                // if the bound lets them.
                                tokio::time::sleep(Duration::from_millis(25)).await;
                                let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
                                let sent = socket.write_all(reply).await.is_ok();
                                active.fetch_sub(1, Ordering::SeqCst);
                                if !sent {
                                    return;
                                }
                            }
                        }
                    });
                }
            });

            let art = Arc::new(ArtHttp {
                endpoint,
                token: "test".to_owned(),
                idle: Mutex::new(Vec::new()),
                in_flight: Semaphore::new(MAX_ART_IN_FLIGHT),
            });
            let mut tasks = Vec::new();
            for i in 0..(MAX_ART_IN_FLIGHT * 3) {
                let art = Arc::clone(&art);
                let id = Id::from_bytes([i as u8; 16]);
                tasks.push(tokio::spawn(async move { art.get(id, 256).await }));
            }
            for task in tasks {
                let got = tokio::time::timeout(Duration::from_secs(60), task).await??;
                assert_eq!(got, Ok(b"ok".to_vec()));
            }
            assert!(peak.load(Ordering::SeqCst) <= MAX_ART_IN_FLIGHT);
            let idle_len = art.idle.lock().map_or(usize::MAX, |idle| idle.len());
            assert!(idle_len <= MAX_ART_IN_FLIGHT);
            server.abort();
            Ok(())
        })
    }

    #[test]
    fn a_cover_the_server_has_not_is_missing_and_one_it_failed_to_give_is_failed()
    -> Result<(), Box<dyn std::error::Error>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let endpoint = listener.local_addr()?;
            // The status asked for is the first byte of the art ID.
            let server = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 512];
                        while let Ok(n) = socket.read(&mut chunk).await {
                            if n == 0 {
                                return;
                            }
                            buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
                            let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                                continue;
                            };
                            let head = String::from_utf8_lossy(buf.get(..end).unwrap_or_default())
                                .into_owned();
                            buf.drain(..end + 4);
                            let status = if head.contains("/01010101") {
                                "404 Not Found"
                            } else if head.contains("/05050505") {
                                "500 Internal Server Error"
                            } else if head.contains("/06060606") {
                                "503 Service Unavailable"
                            } else {
                                "200 OK"
                            };
                            let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\n\r\nok");
                            if socket.write_all(reply.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                    });
                }
            });
            let art = ArtHttp {
                endpoint,
                token: "test".to_owned(),
                idle: Mutex::new(Vec::new()),
                in_flight: Semaphore::new(MAX_ART_IN_FLIGHT),
            };
            let id = |byte| Id::from_bytes([byte; 16]);
            assert_eq!(art.get(id(1), 256).await, Err(NoArt::Missing));
            assert_eq!(art.get(id(5), 256).await, Err(NoArt::Failed));
            assert_eq!(art.get(id(6), 256).await, Err(NoArt::Failed));
            assert_eq!(art.get(id(2), 256).await, Ok(b"ok".to_vec()));
            server.abort();
            let _ = server.await;

            // No server to answer.
            let gone = ArtHttp {
                endpoint,
                token: "test".to_owned(),
                idle: Mutex::new(Vec::new()),
                in_flight: Semaphore::new(MAX_ART_IN_FLIGHT),
            };
            assert_eq!(gone.get(id(2), 256).await, Err(NoArt::Failed));
            Ok(())
        })
    }
}
