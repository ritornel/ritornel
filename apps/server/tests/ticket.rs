//! Contract tests of the socket ticket: how the desktop's page opens the
//! socket, and why no other page can. A real server inside the test
//! process, on a data directory of its own and a port the system picks,
//! as in `contract.rs`. See "Query-string credentials" and "Origin check"
//! in docs/design/contract.md.
//!
//! A ticket belongs to the socket's upgrade, so everything here is over a
//! real socket on loopback. The transport in memory has no upgrade and
//! takes no ticket: a test joins it to a session directly. Once a socket
//! is open, how it was opened makes no difference to the calls on it, and
//! `contract.rs` tests those on both transports.
//!
//! The last test runs the real `ritornel-server` binary, because what
//! must hold of the log can only be seen in a log.
//!
//! Nothing here sleeps, and every test is cut off, with a message, if it
//! has not finished in [`WAIT`].

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ritornel_client::Credential;
use ritornel_client::contract::{SocketTicket, TICKET_PARAM, route};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Server, TICKET_LIFE};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// How long a test may take before it is failed. It is a bound, not a
/// wait: a test that passes never comes near it.
const WAIT: Duration = Duration::from_secs(60);

/// The `Origin` the desktop's page presents.
const DESKTOP: &str = "tauri://localhost";

/// Runs a test, and fails it if it has not finished in [`WAIT`].
async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(WAIT, test)
        .await
        .unwrap_or_else(|_| panic!("the test did not finish within {WAIT:?}"));
}

/// A directory that is removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ritornel-server-ticket-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// The data directory. The server creates it.
    fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The local credential's token, as a client on this machine reads it.
fn token_in(data_dir: &Path) -> String {
    std::fs::read_to_string(data_dir.join("credential"))
        .unwrap()
        .trim()
        .to_owned()
}

/// A server running inside the test, on a directory of its own.
struct Running {
    server: Server,
    scratch: Scratch,
}

impl Running {
    async fn start() -> Self {
        Self::start_with(Bounds::default()).await
    }

    async fn start_with(bounds: Bounds) -> Self {
        let scratch = Scratch::new();
        let server = Server::start_with(
            HeldDir::take(scratch.data_dir()).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            bounds,
        )
        .await
        .unwrap();
        Self { server, scratch }
    }

    fn endpoint(&self) -> SocketAddr {
        self.server.endpoint()
    }

    fn token(&self) -> String {
        token_in(&self.scratch.data_dir())
    }

    fn credential(&self) -> Credential {
        Credential::new(self.token())
    }

    /// A new ticket, asked for as a program on this machine asks.
    async fn ticket(&self) -> String {
        ritornel_client::ticket(self.endpoint(), &self.credential())
            .await
            .unwrap()
            .ticket
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Tries to open the socket with this query string and these headers.
/// `Err` is the HTTP status the server refused with.
async fn upgrade(
    endpoint: SocketAddr,
    query: &str,
    headers: &[(&'static str, &str)],
) -> Result<Socket, u16> {
    let mut request = format!("ws://{endpoint}{}{query}", route::WS)
        .into_client_request()
        .unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(*name, value.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, _)) => Ok(socket),
        Err(tungstenite::Error::Http(response)) => Err(response.status().as_u16()),
        Err(other) => panic!("the upgrade failed before the server answered: {other}"),
    }
}

/// Tries to open the socket with a ticket, as the page of this origin, or
/// as a program if there is none.
async fn with_ticket(
    endpoint: SocketAddr,
    ticket: &str,
    origin: Option<&str>,
) -> Result<Socket, u16> {
    let query = format!("?{TICKET_PARAM}={ticket}");
    match origin {
        Some(origin) => upgrade(endpoint, &query, &[("origin", origin)]).await,
        None => upgrade(endpoint, &query, &[]).await,
    }
}

/// Makes one call on a socket and returns the response's frame.
async fn call(socket: &mut Socket, id: u32, method: &str, params: Value) -> Value {
    let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .unwrap();
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => {
                let frame: Value = serde_json::from_str(text.as_str()).unwrap();
                if frame["id"] == id {
                    return frame;
                }
            }
            other => panic!("expected a frame, and got {other:?}"),
        }
    }
}

fn hello() -> Value {
    json!({ "name": "ticket-test", "kind": "desktop", "version": "1.2.3", "contractVersion": 0 })
}

/// What a raw HTTP request was answered with.
struct Answered {
    status: u16,
    /// The header lines, in lower case.
    headers: Vec<String>,
    body: String,
}

impl Answered {
    /// Whether the answer says anything a browser would take as leave for
    /// a page to make or to read the request.
    fn allows_a_page(&self) -> bool {
        self.headers
            .iter()
            .any(|line| line.starts_with("access-control-"))
    }
}

/// Sends one HTTP request exactly as written, and reads the whole answer.
/// It can send what no client library would: any method, with any headers.
async fn http(
    endpoint: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Answered {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Content-Length: 0\r\n\r\n");

    let mut connection = TcpStream::connect(endpoint).await.unwrap();
    connection.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    connection.read_to_end(&mut answer).await.unwrap();
    let answer = String::from_utf8(answer).unwrap();

    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    let status = lines.next().unwrap().split(' ').nth(1).unwrap();
    Answered {
        status: status.parse().unwrap(),
        headers: lines.map(str::to_ascii_lowercase).collect(),
        body: body.to_owned(),
    }
}

#[tokio::test]
async fn a_ticket_opens_one_socket_of_the_kind_the_credential_holds() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();

        let issued = ritornel_client::ticket(endpoint, &running.credential())
            .await
            .unwrap();
        assert_eq!(issued.ticket.len(), 64);
        assert_eq!(
            u128::from(issued.expires_in_ms),
            TICKET_LIFE.as_millis(),
            "a ticket says how long the server lets it wait"
        );
        // It is not the session token, and says nothing of it.
        assert_ne!(issued.ticket, running.token());

        // No header and no origin: a program that keeps its token out of
        // the socket's URL.
        let mut socket = with_ticket(endpoint, &issued.ticket, None).await.unwrap();
        let descriptor = call(&mut socket, 1, "server.hello", hello()).await;
        assert_eq!(descriptor["result"]["sessionKind"], "full");
        assert_eq!(
            descriptor["result"]["serverId"],
            running.server.server_id().to_string()
        );
        // The local credential holds a full session, so its ticket opens
        // one: the status needs it.
        let status = call(&mut socket, 2, "server.status", json!({})).await;
        assert_eq!(status["result"]["name"], "Ritornel server");

        // It worked once. The socket it opened being open, or closed,
        // changes nothing.
        assert_eq!(
            with_ticket(endpoint, &issued.ticket, None).await.err(),
            Some(401)
        );
        drop(socket);
        assert_eq!(
            with_ticket(endpoint, &issued.ticket, Some(DESKTOP))
                .await
                .err(),
            Some(401)
        );
    })
    .await;
}

#[tokio::test]
async fn the_desktops_page_opens_the_socket_with_a_ticket_and_gets_events() {
    bounded(async {
        let running = Running::start().await;
        let ticket = running.ticket().await;

        let mut socket = with_ticket(running.endpoint(), &ticket, Some(DESKTOP))
            .await
            .unwrap();
        let descriptor = call(&mut socket, 1, "server.hello", hello()).await;
        assert_eq!(descriptor["result"]["sessionKind"], "full");
        let subscribed = call(
            &mut socket,
            2,
            "events.subscribe",
            json!({ "topics": ["servers"] }),
        )
        .await;
        assert_eq!(subscribed["result"]["topics"], json!(["servers"]));

        running.server.rename("desk").unwrap();
        let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
            panic!("expected an event");
        };
        let event: Value = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(event["method"], "event");
        assert_eq!(event["params"]["topic"], "servers");
        assert_eq!(event["params"]["server"]["name"], "desk");
    })
    .await;
}

#[tokio::test]
async fn a_ticket_from_any_other_origin_is_refused_and_is_spent() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();
        let own = format!("http://{endpoint}");
        // A page on the web, pages on this machine, a page with no origin
        // to name, and near misses of the desktop's.
        for origin in [
            "https://example.com",
            own.as_str(),
            "http://127.0.0.1:4747",
            "http://localhost",
            "null",
            "http://tauri.localhost",
            "https://tauri.localhost",
            "tauri://localhost.example.com",
            "tauri://localhost:80",
            "tauri://localhost/",
            "TAURI://LOCALHOST",
            "tauri://localhos",
        ] {
            let ticket = running.ticket().await;
            assert_eq!(
                with_ticket(endpoint, &ticket, Some(origin)).await.err(),
                Some(403),
                "{origin}"
            );
            // A ticket that reached a stranger opens nothing afterwards,
            // from anywhere.
            assert_eq!(
                with_ticket(endpoint, &ticket, Some(DESKTOP)).await.err(),
                Some(401),
                "{origin}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn the_desktops_origin_opens_nothing_without_a_ticket() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();
        let origin = [("origin", DESKTOP)];

        // Every Tauri app has this origin, so it is not a credential.
        assert_eq!(upgrade(endpoint, "", &origin).await.err(), Some(401));
        // A page cannot send the header, so a request with the origin and
        // the header is not the page's, and is refused though the
        // credential is right.
        let authorization = format!("Bearer {}", running.token());
        assert_eq!(
            upgrade(
                endpoint,
                "",
                &[("origin", DESKTOP), ("authorization", &authorization)]
            )
            .await
            .err(),
            Some(403)
        );
        // And it is the ticket that lets the page in: with the header as
        // well, the header is what was presented.
        let ticket = running.ticket().await;
        assert_eq!(
            upgrade(
                endpoint,
                &format!("?{TICKET_PARAM}={ticket}"),
                &[("origin", DESKTOP), ("authorization", &authorization)]
            )
            .await
            .err(),
            Some(403)
        );
        // The header was accepted first, so the ticket was never tried,
        // and still works.
        with_ticket(endpoint, &ticket, Some(DESKTOP)).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn an_expired_ticket_is_refused() {
    bounded(async {
        // A ticket with no time to wait has run out by the time it is
        // presented, however soon that is.
        let running = Running::start_with(Bounds {
            ticket_life: Duration::ZERO,
            ..Bounds::default()
        })
        .await;
        let endpoint = running.endpoint();
        let issued = ritornel_client::ticket(endpoint, &running.credential())
            .await
            .unwrap();
        assert_eq!(issued.expires_in_ms, 0);

        for origin in [None, Some(DESKTOP)] {
            assert_eq!(
                with_ticket(endpoint, &issued.ticket, origin).await.err(),
                Some(401)
            );
        }
        // The credential still opens the socket: only tickets ran out.
        let authorization = format!("Bearer {}", running.token());
        upgrade(endpoint, "", &[("authorization", &authorization)])
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_made_up_ticket_is_refused_and_the_credential_is_not_a_ticket() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();
        let real = running.ticket().await;
        let token = running.token();

        for made_up in [
            String::new(),
            "0".repeat(64),
            "f".repeat(64),
            real[1..].to_owned(),
            format!("{real}0"),
            real.to_uppercase(),
            "not-a-ticket".to_owned(),
            // The session token never rides in a URL, so it opens nothing
            // from one.
            token.clone(),
        ] {
            // An upper-case copy of a ticket that is all digits is the
            // ticket. It has never happened.
            if made_up == real {
                continue;
            }
            for origin in [None, Some(DESKTOP)] {
                assert_eq!(
                    with_ticket(endpoint, &made_up, origin).await.err(),
                    Some(401),
                    "a ticket of {} characters",
                    made_up.len()
                );
            }
        }
        // Other ways of writing a query string that are not a ticket.
        for query in [
            "?",
            "?ticket",
            "?tickets=1",
            &format!("?token={token}"),
            &format!("?Ticket={real}"),
            &format!("?x{TICKET_PARAM}={real}"),
        ] {
            assert_eq!(
                upgrade(endpoint, query, &[]).await.err(),
                Some(401),
                "{query}"
            );
        }
        // And a ticket is not a credential: it goes in no header.
        let as_header = format!("Bearer {real}");
        assert_eq!(
            upgrade(endpoint, "", &[("authorization", &as_header)])
                .await
                .err(),
            Some(401)
        );
        assert_eq!(
            http(
                endpoint,
                "GET",
                route::HEALTH,
                &[("Authorization", &as_header)]
            )
            .await
            .status,
            401
        );

        // None of that spent the real one, which still works, among other
        // parameters too.
        upgrade(endpoint, &format!("?a=b&{TICKET_PARAM}={real}&c"), &[])
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_ticket_opens_the_socket_and_no_other_route() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();
        let ticket = running.ticket().await;
        let query = format!("?{TICKET_PARAM}={ticket}");

        for (method, path) in [
            ("GET", route::HEALTH),
            ("POST", route::TICKET),
            ("GET", "/v1/nothing"),
            // The socket's route takes a ticket on a `GET` alone.
            ("POST", route::WS),
        ] {
            let answer = http(endpoint, method, &format!("{path}{query}"), &[]).await;
            assert_eq!(answer.status, 401, "{method} {path}");
            assert!(!answer.allows_a_page(), "{method} {path}");
        }
        // It was not tried on any of them, so it is not spent.
        with_ticket(endpoint, &ticket, Some(DESKTOP)).await.unwrap();

        // On the socket's route it is tried, and so spent, even by a
        // request that is not an upgrade and opens nothing.
        let ticket = running.ticket().await;
        let plain = http(
            endpoint,
            "GET",
            &format!("{}?{TICKET_PARAM}={ticket}", route::WS),
            &[],
        )
        .await;
        assert!((400..500).contains(&plain.status), "{}", plain.status);
        assert_eq!(
            with_ticket(endpoint, &ticket, Some(DESKTOP)).await.err(),
            Some(401)
        );
    })
    .await;
}

#[tokio::test]
async fn a_ticket_is_made_only_for_the_credential_in_a_header() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();
        let token = running.token();
        let authorization = format!("Bearer {token}");

        // With the credential: a ticket, which nothing may keep.
        let made = http(
            endpoint,
            "POST",
            route::TICKET,
            &[("Authorization", &authorization)],
        )
        .await;
        assert_eq!(made.status, 200);
        assert!(
            made.headers.contains(&"cache-control: no-store".to_owned()),
            "{:?}",
            made.headers
        );
        assert!(!made.allows_a_page());
        let issued: SocketTicket = serde_json::from_str(&made.body).unwrap();
        with_ticket(endpoint, &issued.ticket, Some(DESKTOP))
            .await
            .unwrap();

        // Without it, or with it anywhere but the header: nothing.
        let wrong = format!("Bearer {}", "0".repeat(64));
        let in_the_query = format!("{}?token={token}", route::TICKET);
        let as_a_ticket = format!("{}?{TICKET_PARAM}={token}", route::TICKET);
        for (path, headers) in [
            (route::TICKET, vec![]),
            (route::TICKET, vec![("Authorization", wrong.as_str())]),
            (route::TICKET, vec![("Authorization", token.as_str())]),
            (route::TICKET, vec![("Cookie", token.as_str())]),
            (in_the_query.as_str(), vec![]),
            (as_a_ticket.as_str(), vec![]),
        ] {
            let refused = http(endpoint, "POST", path, &headers).await;
            assert_eq!(refused.status, 401, "{path} {}", headers.len());
            assert!(!refused.body.contains("ticket\":"), "{}", refused.body);
            assert!(!refused.allows_a_page());
        }
        // The client reports the same refusal.
        assert!(matches!(
            ritornel_client::ticket(endpoint, &Credential::new("0".repeat(64))).await,
            Err(ritornel_client::Error::Refused { status: 401 })
        ));

        // Asking makes something, so it is a `POST` and nothing else.
        for method in ["GET", "PUT", "DELETE"] {
            let refused = http(
                endpoint,
                method,
                route::TICKET,
                &[("Authorization", &authorization)],
            )
            .await;
            assert_eq!(refused.status, 405, "{method}");
            assert!(!refused.body.contains("ticket\":"), "{method}");
        }
    })
    .await;
}

#[tokio::test]
async fn a_page_cannot_ask_for_a_ticket() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();
        let authorization = format!("Bearer {}", running.token());

        for origin in [DESKTOP, "https://example.com", "http://localhost", "null"] {
            // What a browser sends first for a request with an
            // `Authorization` header. It carries no credential, and it is
            // answered like any other request without one: with nothing
            // that would let the page go on to send the request itself.
            let preflight = http(
                endpoint,
                "OPTIONS",
                route::TICKET,
                &[
                    ("Origin", origin),
                    ("Access-Control-Request-Method", "POST"),
                    ("Access-Control-Request-Headers", "authorization"),
                ],
            )
            .await;
            assert_eq!(preflight.status, 401, "{origin}");
            assert!(
                !preflight.allows_a_page(),
                "{origin}: {:?}",
                preflight.headers
            );

            // And if the request itself came, named for a page and with
            // the right credential, it would still get no ticket: a
            // program asks for one, and a program names no origin.
            let asked = http(
                endpoint,
                "POST",
                route::TICKET,
                &[("Origin", origin), ("Authorization", &authorization)],
            )
            .await;
            assert_eq!(asked.status, 403, "{origin}");
            assert!(
                !asked.body.contains("ticket\":"),
                "{origin}: {}",
                asked.body
            );
            assert!(!asked.allows_a_page(), "{origin}: {:?}", asked.headers);
        }

        // The socket's route answers a preflight the same way.
        let preflight = http(
            endpoint,
            "OPTIONS",
            route::WS,
            &[
                ("Origin", "https://example.com"),
                ("Access-Control-Request-Method", "GET"),
            ],
        )
        .await;
        assert_eq!(preflight.status, 401);
        assert!(!preflight.allows_a_page());
    })
    .await;
}

/// The real `ritornel-server`, started on `data_dir` with the most
/// verbose log there is, and killed if the test ends without stopping it.
struct Process(std::process::Child);

impl Process {
    /// Starts the server and waits for its ready line.
    fn start(data_dir: &Path) -> (Self, std::sync::mpsc::Receiver<String>) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ritornel-server"))
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .args(["--listen", "127.0.0.1:0", "--log-level", "trace"])
            .env_remove("RITORNEL_DATA_DIR")
            .env_remove("RITORNEL_LISTEN")
            .env_remove("RITORNEL_LOG_LEVEL")
            .env_remove("RITORNEL_BACKUP_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Everything it prints, on both pipes, a line at a time. The
        // channel closes when the process has ended.
        let (send, printed) = std::sync::mpsc::channel();
        let pipes: [Box<dyn std::io::Read + Send>; 2] = [
            Box::new(child.stdout.take().unwrap()),
            Box::new(child.stderr.take().unwrap()),
        ];
        for pipe in pipes {
            let send = send.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines() {
                    if send.send(line.unwrap()).is_err() {
                        break;
                    }
                }
            });
        }
        let process = Self(child);
        let ready = printed.recv_timeout(WAIT).expect("no ready line");
        assert!(ready.starts_with("ready pid="), "{ready}");
        (process, printed)
    }

    /// Asks the server to stop and waits until it has.
    fn stop(mut self) {
        // SIGTERM, or on Windows `taskkill /PID` without `/F`, which asks
        // the server's hidden window to close.
        let asked = if cfg!(windows) {
            Command::new("taskkill")
                .args(["/PID", &self.0.id().to_string()])
                .output()
                .unwrap()
                .status
        } else {
            Command::new("kill")
                .args(["-TERM", &self.0.id().to_string()])
                .status()
                .unwrap()
        };
        assert!(asked.success());
        assert!(self.0.wait().unwrap().success());
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // After `stop` these do nothing.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn neither_a_ticket_nor_the_credential_is_in_anything_the_server_writes() {
    bounded(async {
        let scratch = Scratch::new();
        let data_dir = scratch.data_dir();
        let (process, printed) = Process::start(&data_dir);
        let found = ritornel_client::local::find(&data_dir).await.unwrap();
        let endpoint = found.endpoint;
        let token = token_in(&data_dir);

        // Every way a ticket passes through the server: used by the page,
        // used a second time, presented by a stranger, expired by being
        // spent, and one that is never used at all.
        let used = found.ticket().await.unwrap().ticket;
        let mut socket = with_ticket(endpoint, &used, Some(DESKTOP)).await.unwrap();
        call(&mut socket, 1, "server.hello", hello()).await;
        call(&mut socket, 2, "server.status", json!({})).await;
        assert_eq!(with_ticket(endpoint, &used, None).await.err(), Some(401));

        let stolen = found.ticket().await.unwrap().ticket;
        assert_eq!(
            with_ticket(endpoint, &stolen, Some("https://example.com"))
                .await
                .err(),
            Some(403)
        );
        let unused = found.ticket().await.unwrap().ticket;
        let made_up = "5e471e15e471e15e471e15e471e15e471e15e471e15e471e15e471e15e471e10";
        assert_eq!(with_ticket(endpoint, made_up, None).await.err(), Some(401));
        // The credential in the wrong places, where a careless log would
        // repeat it.
        let authorization = format!("Bearer {token}");
        let refused = http(
            endpoint,
            "POST",
            &format!("{}?token={token}", route::TICKET),
            &[
                ("Origin", "https://example.com"),
                ("Authorization", &authorization),
            ],
        )
        .await;
        assert_eq!(refused.status, 403);

        drop(socket);
        process.stop();

        let mut written = String::new();
        // The channel closes once both pipes are at their end.
        while let Ok(line) = printed.recv_timeout(WAIT) {
            written.push_str(&line);
            written.push('\n');
        }
        let mut log_lines = 0;
        for file in std::fs::read_dir(data_dir.join("logs")).unwrap() {
            let text = std::fs::read_to_string(file.unwrap().path()).unwrap();
            log_lines += text.lines().count();
            written.push_str(&text);
        }
        // The log was being written, at its most verbose, and saw the
        // session the ticket opened: this is not passing because nothing
        // was logged.
        assert!(log_lines > 5, "{written}");
        assert!(written.contains("\"client\":\"ticket-test\""), "{written}");
        assert!(written.contains("\"level\":\"debug\""), "{written}");

        for (what, secret) in [
            ("the ticket that was used", used.as_str()),
            ("the ticket a stranger presented", stolen.as_str()),
            ("the ticket that was never used", unused.as_str()),
            ("the ticket that was made up", made_up),
            ("the credential", token.as_str()),
        ] {
            assert!(!written.contains(secret), "{what} was written");
        }
        // Nor is anything else of a request: the server logs no URLs.
        for part in [format!("{TICKET_PARAM}="), "/v1/".to_owned()] {
            assert!(!written.contains(&part), "{part}: {written}");
        }
        // No data directory is left with anything in it that opens a
        // socket but the credential file.
        for name in ["server.json", "server.lock", "config.toml"] {
            if let Ok(text) = std::fs::read_to_string(data_dir.join(name)) {
                assert!(!text.contains(&unused) && !text.contains(&token), "{name}");
            }
        }
    })
    .await;
}
