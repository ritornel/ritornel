//! What carries frames between the client and a server: a socket, or a
//! pair of channels in memory. Both carry a frame as text, so the encoding
//! is the same and only the transport differs (docs/design/contract.md,
//! decision 4).

use std::net::SocketAddr;

use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use ritornel_contract::route;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::error::transport;
use crate::{Credential, Error, http};

pub(crate) enum Transport {
    Socket(Box<WebSocketStream<TcpStream>>),
    Memory {
        outgoing: Sender<String>,
        incoming: Receiver<String>,
    },
}

impl Transport {
    /// Opens the socket at `endpoint`, presenting the credential in an
    /// `Authorization` header on the upgrade.
    pub(crate) async fn connect(
        endpoint: SocketAddr,
        credential: &Credential,
    ) -> Result<Self, Error> {
        let stream = http::connect(endpoint).await?;
        Self::upgrade(stream, endpoint, credential).await
    }

    /// Opens the socket over a connection that is already made to
    /// `endpoint`. The credential is first sent here, so a caller that has
    /// something to check about who is at the other end checks it between
    /// connecting and this.
    pub(crate) async fn upgrade(
        stream: TcpStream,
        endpoint: SocketAddr,
        credential: &Credential,
    ) -> Result<Self, Error> {
        let mut request = format!("ws://{endpoint}{}", route::WS)
            .into_client_request()
            .map_err(transport)?;
        request
            .headers_mut()
            .insert(AUTHORIZATION, http::authorization(credential)?);
        http::within_deadline(async {
            match tokio_tungstenite::client_async(request, stream).await {
                Ok((socket, _)) => Ok(Self::Socket(Box::new(socket))),
                Err(tungstenite::Error::Http(response)) => Err(Error::Refused {
                    status: response.status().as_u16(),
                }),
                Err(other) => Err(transport(other)),
            }
        })
        .await
    }

    pub(crate) fn split(self) -> (Writer, Reader) {
        match self {
            Self::Socket(socket) => {
                let (writer, reader) = (*socket).split();
                (Writer::Socket(writer), Reader::Socket(reader))
            }
            Self::Memory { outgoing, incoming } => {
                (Writer::Memory(outgoing), Reader::Memory(incoming))
            }
        }
    }
}

pub(crate) enum Writer {
    Socket(SplitSink<WebSocketStream<TcpStream>, Message>),
    Memory(Sender<String>),
}

pub(crate) enum Reader {
    Socket(SplitStream<WebSocketStream<TcpStream>>),
    Memory(Receiver<String>),
}

impl Writer {
    /// Sends one frame.
    pub(crate) async fn send(&mut self, text: String) -> Result<(), Error> {
        match self {
            Self::Socket(socket) => socket
                .send(Message::Text(text.into()))
                .await
                .map_err(transport),
            Self::Memory(outgoing) => outgoing
                .send(text)
                .await
                .map_err(|_| transport("the connection closed")),
        }
    }

    pub(crate) async fn close(&mut self) {
        if let Self::Socket(socket) = self {
            let _ = socket.close().await;
        }
    }
}

impl Reader {
    /// The next frame. `None` when the other side has closed.
    pub(crate) async fn receive(&mut self) -> Option<Result<String, Error>> {
        match self {
            Self::Socket(socket) => loop {
                return match socket.next().await? {
                    Ok(Message::Text(text)) => Some(Ok(text.as_str().to_owned())),
                    // The socket answers a ping by itself.
                    Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => continue,
                    Ok(Message::Close(_)) => None,
                    Ok(Message::Binary(_)) => Some(Err(Error::Protocol(
                        "a frame is text, and the server sent bytes".to_owned(),
                    ))),
                    Err(error) => Some(Err(transport(error))),
                };
            },
            Self::Memory(incoming) => incoming.recv().await.map(Ok),
        }
    }
}
