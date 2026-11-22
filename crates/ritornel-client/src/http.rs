//! The HTTP side of the contract: the routes that are not the socket.

use std::net::SocketAddr;
use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use ritornel_contract::route::{self, Health};
use ritornel_contract::{PublicDescriptor, SocketTicket};
use tokio::net::TcpStream;

use crate::error::transport;
use crate::{Credential, Error};

/// The largest body the client reads from one of these routes. What they
/// answer is small, and something else might be answering.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// A local connection, upgrade, or HTTP exchange should answer promptly.
const IO_WAIT: Duration = Duration::from_secs(10);

pub(crate) async fn within_deadline<T>(
    operation: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(IO_WAIT, operation)
        .await
        .map_err(|_| transport("timed out waiting for the server"))?
}

pub(crate) async fn connect(endpoint: SocketAddr) -> Result<TcpStream, Error> {
    within_deadline(async { TcpStream::connect(endpoint).await.map_err(transport) }).await
}

/// What a request was answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The HTTP status.
    pub status: u16,
    /// The body.
    pub body: Vec<u8>,
}

/// Sends `GET <path>` to `endpoint`, with the credential if there is one,
/// and returns whatever came back.
pub async fn get(
    endpoint: SocketAddr,
    path: &str,
    credential: Option<&Credential>,
) -> Result<Answer, Error> {
    let stream = connect(endpoint).await?;
    send(stream, endpoint, Method::GET, path, credential, None).await
}

/// Sends `POST <path>` to `endpoint` with `body` as JSON, with the
/// credential if there is one, and returns whatever came back.
pub async fn post(
    endpoint: SocketAddr,
    path: &str,
    credential: Option<&Credential>,
    body: &impl serde::Serialize,
) -> Result<Answer, Error> {
    let body = serde_json::to_vec(body)
        .map_err(|error| Error::Protocol(format!("a body for POST {path}: {error}")))?;
    let stream = connect(endpoint).await?;
    send(stream, endpoint, Method::POST, path, credential, Some(body)).await
}

/// Sends one request on a connection that is already made, with the
/// credential if there is one and a JSON body if there is one, and returns
/// whatever came back. Nothing has been sent on the connection yet.
async fn send(
    stream: TcpStream,
    endpoint: SocketAddr,
    method: Method,
    path: &str,
    credential: Option<&Credential>,
    json: Option<Vec<u8>>,
) -> Result<Answer, Error> {
    within_deadline(async {
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(transport)?;
        // The connection does its reading and writing here, and ends when the
        // sender is dropped.
        tokio::spawn(connection);

        let mut request = Request::builder()
            .method(&method)
            .uri(path)
            .header(HOST, endpoint.to_string());
        if let Some(credential) = credential {
            request = request.header(AUTHORIZATION, authorization(credential)?);
        }
        if json.is_some() {
            request = request.header(CONTENT_TYPE, "application/json");
        }
        let request = request
            .body(Full::new(Bytes::from(json.unwrap_or_default())))
            .map_err(transport)?;
        let response = sender.send_request(request).await.map_err(transport)?;
        let status = response.status().as_u16();
        let body = Limited::new(response.into_body(), MAX_BODY_BYTES)
            .collect()
            .await
            .map_err(|_| {
                Error::Protocol(format!(
                    "the answer to {method} {path} was cut off or too long"
                ))
            })?
            .to_bytes()
            .to_vec();
        Ok(Answer { status, body })
    })
    .await
}

/// The `Authorization` header that carries a credential. It is marked
/// sensitive, so the HTTP library keeps it out of what it prints.
pub(crate) fn authorization(credential: &Credential) -> Result<HeaderValue, Error> {
    let mut value = HeaderValue::from_str(&format!("Bearer {}", credential.expose()))
        // A token the server issued is hex digits. This one is not, so no
        // server issued it.
        .map_err(|_| Error::Refused { status: 401 })?;
    value.set_sensitive(true);
    Ok(value)
}

/// Reads the body of a route that answered 200 as the type the contract
/// gives it.
fn read<T: serde::de::DeserializeOwned>(path: &str, answer: &Answer) -> Result<T, Error> {
    match StatusCode::from_u16(answer.status) {
        Ok(StatusCode::OK) => serde_json::from_slice(&answer.body)
            .map_err(|error| Error::Protocol(format!("the answer to GET {path}: {error}"))),
        Ok(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => Err(Error::Refused {
            status: answer.status,
        }),
        _ => Err(Error::Protocol(format!(
            "GET {path} was answered with HTTP {}",
            answer.status
        ))),
    }
}

/// Asks whatever listens at `endpoint` for its public descriptor. It needs
/// no credential, so a client can check which server answered before it
/// sends one.
pub async fn descriptor(endpoint: SocketAddr) -> Result<PublicDescriptor, Error> {
    read(
        route::DESCRIPTOR,
        &get(endpoint, route::DESCRIPTOR, None).await?,
    )
}

/// Asks the server at `endpoint` whether it is up.
pub async fn health(endpoint: SocketAddr, credential: &Credential) -> Result<Health, Error> {
    read(
        route::HEALTH,
        &get(endpoint, route::HEALTH, Some(credential)).await?,
    )
}

/// Trades the credential for a ticket that opens one socket of the server
/// at `endpoint`: `POST /v1/ticket`. A program asks for one on behalf of
/// something that cannot send the credential itself, as the desktop's
/// shell does for its page.
///
/// The credential is sent to whatever listens at `endpoint`.
/// [`LocalServer::ticket`](crate::local::LocalServer::ticket) makes sure
/// that is the server, and this does not.
pub async fn ticket(endpoint: SocketAddr, credential: &Credential) -> Result<SocketTicket, Error> {
    let stream = connect(endpoint).await?;
    ticket_over(stream, endpoint, credential).await
}

/// As [`ticket`], over a connection that is already made. Nothing has been
/// sent on it yet, and the credential is the first thing that is.
pub(crate) async fn ticket_over(
    stream: TcpStream,
    endpoint: SocketAddr,
    credential: &Credential,
) -> Result<SocketTicket, Error> {
    let answer = send(
        stream,
        endpoint,
        Method::POST,
        route::TICKET,
        Some(credential),
        None,
    )
    .await?;
    let path = route::TICKET;
    match StatusCode::from_u16(answer.status) {
        // Why a body could not be read is not passed on: the reason may
        // quote the body, and this body holds a ticket.
        Ok(StatusCode::OK) => serde_json::from_slice(&answer.body)
            .map_err(|_| Error::Protocol(format!("the answer to POST {path} is not a ticket"))),
        Ok(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => Err(Error::Refused {
            status: answer.status,
        }),
        _ => Err(Error::Protocol(format!(
            "POST {path} was answered with HTTP {}",
            answer.status
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn http_and_socket_upgrades_time_out_when_the_peer_never_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let key = Credential::new("test");
        let session = async {
            tokio::join!(
                get(endpoint, "/", None),
                crate::Client::connect(endpoint, &key)
            )
        };
        let (http, ws) = tokio::time::timeout(std::time::Duration::from_secs(60), session)
            .await
            .unwrap();
        assert!(matches!(http, Err(Error::Transport(_))));
        assert!(matches!(ws, Err(Error::Transport(_))));
    }
}
