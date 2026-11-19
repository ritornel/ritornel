//! A connection to one server: typed calls, and the events it sends.

use std::collections::HashMap;
use std::net::SocketAddr;

use ritornel_contract::methods::{
    EventsSubscribe, EventsUnsubscribe, JobsCancel, JobsGet, JobsList, LibraryCreate, LibraryStats,
    ListClose, ListLocate, ListOpen, ListSectionsCall, ListWindowCall, MediaTokenCall, Method,
    ScanStart, ServerHello, ServerStatus, ServersList, SourcesAdd, SourcesList,
};
use ritornel_contract::{
    Descriptor, EVENT_METHOD, Event, Frame, HelloParams, Id, JobCancelParams, JobCancelled,
    JobDetail, JobGetParams, LibraryCreateParams, LibraryCreated, ListClosed, ListLocateParams,
    ListLocated, ListOpenParams, ListOpened, ListSections, ListWindow, ListWindowParams,
    MediaToken, NoParams, Request, RequestId, Response, ScanStartParams, ScanStarted, SourceAdded,
    SourcesAddParams, Subscriptions, Topic, TopicsParams, ViewParams,
};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use crate::error::transport;
use crate::transport::Transport;
use crate::{Credential, Error};

/// A call on its way to the server, and where its answer goes.
struct Call {
    method: String,
    params: Value,
    answer: oneshot::Sender<Result<Value, Error>>,
}

/// A connection to one server.
///
/// Calls may be made from several tasks at once: each gets the response
/// that carries its own ID. Events are read from the same value, in the
/// order the server sent them, and an event the server sent before a
/// response is there to be read once that response has arrived.
///
/// Dropping it closes the connection.
#[derive(Debug)]
pub struct Client {
    calls: UnboundedSender<Call>,
    events: UnboundedReceiver<Event>,
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Params may hold anything, so they are left out.
        f.debug_struct("Call")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Opens the socket of the server at `endpoint` and presents the
    /// credential. The first call must then be [`hello`](Self::hello).
    ///
    /// It must be called on a Tokio runtime, and the connection runs on
    /// that runtime.
    pub async fn connect(endpoint: SocketAddr, credential: &Credential) -> Result<Self, Error> {
        Ok(Self::start(Transport::connect(endpoint, credential).await?))
    }

    /// As [`connect`](Self::connect), over a connection that is already
    /// made. Nothing has been sent on it yet, and the credential is the
    /// first thing that is.
    pub(crate) async fn upgrade(
        stream: TcpStream,
        endpoint: SocketAddr,
        credential: &Credential,
    ) -> Result<Self, Error> {
        Ok(Self::start(
            Transport::upgrade(stream, endpoint, credential).await?,
        ))
    }

    /// Joins the client to a server in the same process through a pair of
    /// channels: where the client sends frames, as text, and where it
    /// receives them. Tests use it. Every frame is still encoded and
    /// decoded.
    ///
    /// It must be called on a Tokio runtime.
    pub fn over_memory(outgoing: mpsc::Sender<String>, incoming: mpsc::Receiver<String>) -> Self {
        Self::start(Transport::Memory { outgoing, incoming })
    }

    fn start(transport: Transport) -> Self {
        let (calls, queued) = mpsc::unbounded_channel();
        let (events, received) = mpsc::unbounded_channel();
        tokio::spawn(run(transport, queued, events));
        Self {
            calls,
            events: received,
        }
    }

    /// Calls a method by name with params of any shape, and returns its
    /// result as JSON. The typed methods are built on it. It is also how a
    /// program calls a method this crate has no type for.
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<Value, Error> {
        let (answer, answered) = oneshot::channel();
        let call = Call {
            method: method.to_owned(),
            params,
            answer,
        };
        self.calls
            .send(call)
            .map_err(|_| transport("the connection closed"))?;
        answered
            .await
            .map_err(|_| transport("the connection closed"))?
    }

    /// Calls a method of the contract, with the params and the result the
    /// method table gives it.
    pub async fn call<M: Method>(&self, params: &M::Params) -> Result<M::Result, Error> {
        let params = serde_json::to_value(params)
            .map_err(|error| Error::Protocol(format!("the params of {}: {error}", M::NAME)))?;
        let result = self.call_raw(M::NAME, params).await?;
        serde_json::from_value(result)
            .map_err(|error| Error::Protocol(format!("the result of {}: {error}", M::NAME)))
    }

    /// `server.hello`: says who this program is, and returns the server's
    /// descriptor. It is the first call on a connection.
    pub async fn hello(&self, hello: &HelloParams) -> Result<Descriptor, Error> {
        self.call::<ServerHello>(hello).await
    }

    /// `server.status`: the health of the server.
    pub async fn status(&self) -> Result<ritornel_contract::ServerStatus, Error> {
        self.call::<ServerStatus>(&NoParams {}).await
    }

    /// `servers.list`: every server this one knows, itself included.
    pub async fn servers(&self) -> Result<ritornel_contract::ServersList, Error> {
        self.call::<ServersList>(&NoParams {}).await
    }

    /// `events.subscribe`: asks to be sent events on these topics, and
    /// returns every topic the session then has.
    pub async fn subscribe(&self, topics: &[Topic]) -> Result<Subscriptions, Error> {
        let topics = topics.to_vec();
        self.call::<EventsSubscribe>(&TopicsParams { topics }).await
    }

    /// `events.unsubscribe`: asks for no more events on these topics, and
    /// returns every topic the session then has. No event on them follows
    /// the response.
    pub async fn unsubscribe(&self, topics: &[Topic]) -> Result<Subscriptions, Error> {
        let topics = topics.to_vec();
        self.call::<EventsUnsubscribe>(&TopicsParams { topics })
            .await
    }

    /// `library.create`: creates a library on the server and makes the
    /// server its hub. The key is the command's: sent again with the same
    /// key, the call returns what it returned the first time.
    pub async fn create_library(&self, idempotency_key: Id) -> Result<LibraryCreated, Error> {
        self.call::<LibraryCreate>(&LibraryCreateParams { idempotency_key })
            .await
    }

    /// `sources.list`: every source of the library, with where its root is
    /// on the server.
    pub async fn sources(&self) -> Result<ritornel_contract::SourcesList, Error> {
        self.call::<SourcesList>(&NoParams {}).await
    }

    /// `sources.add`: adds a folder on the server as a named root. The
    /// server only ever reads it.
    pub async fn add_source(
        &self,
        idempotency_key: Id,
        name: &str,
        path: &str,
    ) -> Result<SourceAdded, Error> {
        self.call::<SourcesAdd>(&SourcesAddParams {
            idempotency_key,
            name: name.to_owned(),
            path: path.to_owned(),
        })
        .await
    }

    /// `scan.start`: starts a scan of a source as a job. A source that is
    /// being scanned answers with the job that is running.
    pub async fn start_scan(
        &self,
        idempotency_key: Id,
        source_id: Id,
    ) -> Result<ScanStarted, Error> {
        self.call::<ScanStart>(&ScanStartParams {
            idempotency_key,
            source_id,
        })
        .await
    }

    /// `jobs.list`: the newest jobs, newest first.
    pub async fn jobs(&self) -> Result<ritornel_contract::JobsList, Error> {
        self.call::<JobsList>(&NoParams {}).await
    }

    /// `jobs.get`: one job, with the first of its items that failed.
    pub async fn job(&self, job_id: Id) -> Result<JobDetail, Error> {
        self.call::<JobsGet>(&JobGetParams { job_id }).await
    }

    /// `jobs.cancel`: asks a job to stop.
    pub async fn cancel_job(&self, idempotency_key: Id, job_id: Id) -> Result<JobCancelled, Error> {
        self.call::<JobsCancel>(&JobCancelParams {
            idempotency_key,
            job_id,
        })
        .await
    }

    /// `library.stats`: what the library holds and what the scans made of
    /// it.
    pub async fn library_stats(&self) -> Result<ritornel_contract::LibraryStats, Error> {
        self.call::<LibraryStats>(&NoParams {}).await
    }

    /// `list.open`: resolves a query into a view, and answers with its ID,
    /// its total and the window asked for.
    pub async fn list_open(&self, params: &ListOpenParams) -> Result<ListOpened, Error> {
        self.call::<ListOpen>(params).await
    }

    /// `list.window`: rows of a view, read as the library has them now.
    pub async fn list_window(
        &self,
        view: &str,
        offset: u64,
        limit: u64,
    ) -> Result<ListWindow, Error> {
        let params = ListWindowParams {
            view: view.to_owned(),
            offset,
            limit,
        };
        self.call::<ListWindowCall>(&params).await
    }

    /// `list.sections`: where each letter, year or stretch of time begins.
    pub async fn list_sections(&self, view: &str) -> Result<ListSections, Error> {
        let view = view.to_owned();
        self.call::<ListSectionsCall>(&ViewParams { view }).await
    }

    /// `list.locate`: a row's index, by its ID or the start of its name.
    pub async fn list_locate(&self, params: &ListLocateParams) -> Result<ListLocated, Error> {
        self.call::<ListLocate>(params).await
    }

    /// `list.close`: ends a view.
    pub async fn list_close(&self, view: &str) -> Result<ListClosed, Error> {
        let view = view.to_owned();
        self.call::<ListClose>(&ViewParams { view }).await
    }

    /// `media.token`: the token that reads covers from the query string.
    pub async fn media_token(&self) -> Result<MediaToken, Error> {
        self.call::<MediaTokenCall>(&NoParams {}).await
    }

    /// The next event, waiting until there is one. `None` once the
    /// connection has closed and every event it carried has been read.
    ///
    /// There is no replay: after a lost connection a client connects,
    /// subscribes and reads everything again.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// The next event if one has arrived, without waiting.
    pub fn try_next_event(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Takes the events, for a program that reads them on a task of their
    /// own while the client is shared for calls (calls take `&self`, and
    /// reading an event takes `&mut self`). They arrive in the order the
    /// server sent them, and the stream ends when the connection closes,
    /// which is how such a program learns that it has. After this,
    /// [`next_event`](Self::next_event) has nothing more to give.
    pub fn take_events(&mut self) -> UnboundedReceiver<Event> {
        let (_, closed) = mpsc::unbounded_channel();
        std::mem::replace(&mut self.events, closed)
    }
}

/// Owns the transport: sends each call, and hands each frame that arrives
/// to whoever it is for. It ends when the client is dropped or the
/// connection is lost.
async fn run(
    mut transport: Transport,
    mut calls: UnboundedReceiver<Call>,
    events: UnboundedSender<Event>,
) {
    let mut waiting = HashMap::new();
    let mut last_id = 0;
    let lost = loop {
        tokio::select! {
            call = calls.recv() => {
                let Some(call) = call else {
                    // The client was dropped.
                    transport.close().await;
                    return;
                };
                last_id += 1;
                let frame = Frame::Request(Request {
                    id: RequestId::Number(last_id),
                    method: call.method,
                    params: Some(call.params),
                });
                waiting.insert(last_id, call.answer);
                if let Err(lost) = transport.send(frame.encode()).await {
                    break lost.to_string();
                }
            }
            received = transport.receive() => {
                let text = match received {
                    Some(Ok(text)) => text,
                    Some(Err(lost)) => break lost.to_string(),
                    None => break "the connection closed".to_owned(),
                };
                match Frame::decode(&text) {
                    Ok(Frame::Response(Response { id: Some(RequestId::Number(id)), outcome })) => {
                        if let Some(answer) = waiting.remove(&id) {
                            // The caller may have stopped waiting.
                            let _ = answer.send(outcome.map_err(Error::Call));
                        }
                    }
                    Ok(Frame::Notification(notification)) if notification.method == EVENT_METHOD => {
                        // An event on a topic this program does not know is
                        // not for it.
                        if let Some(event) = notification.params.and_then(known) {
                            // The client may have stopped reading events.
                            let _ = events.send(event);
                        }
                    }
                    // A response to no call of this client's, or a call or
                    // a notification this program does not know: nothing
                    // to do with it.
                    Ok(_) => {}
                    Err(_) => break "the server sent something that is not a frame".to_owned(),
                }
            }
        }
    };
    // Whoever is still waiting learns that no answer will come. Calls made
    // from here on find the channel closed.
    for (_, answer) in waiting {
        let _ = answer.send(Err(Error::Transport(lost.clone())));
    }
}

/// The event these params are, if it is one this program knows.
fn known(params: Value) -> Option<Event> {
    serde_json::from_value(params).ok()
}

#[cfg(test)]
mod tests {
    use ritornel_contract::{ClientKind, ErrorCode, Id, RpcError};
    use serde_json::json;
    use tokio::sync::mpsc::{Receiver, Sender};

    use super::*;

    /// A client, and the two ends a server would hold.
    fn pair() -> (Client, Receiver<String>, Sender<String>) {
        let (outgoing, from_client) = mpsc::channel(8);
        let (to_client, incoming) = mpsc::channel(8);
        (
            Client::over_memory(outgoing, incoming),
            from_client,
            to_client,
        )
    }

    /// The next request the client sent: its ID, method and params.
    async fn sent(from_client: &mut Receiver<String>) -> (i64, String, Value) {
        let Frame::Request(request) = Frame::decode(&from_client.recv().await.unwrap()).unwrap()
        else {
            panic!("the client sends requests");
        };
        let RequestId::Number(id) = request.id else {
            panic!("the client numbers its requests");
        };
        (id, request.method, request.params.unwrap())
    }

    fn result(id: i64, result: Value) -> String {
        Frame::Response(Response {
            id: Some(RequestId::Number(id)),
            outcome: Ok(result),
        })
        .encode()
    }

    fn server_event(revision: u64) -> String {
        json!({ "jsonrpc": "2.0", "method": "event", "params": {
            "topic": "servers",
            "server": {
                "serverId": Id::from_bytes([0x11; 16]),
                "name": "desk",
                "role": "unassigned",
                "isThisServer": true,
                "reachable": true,
                "revision": revision,
            },
        }})
        .to_string()
    }

    #[tokio::test]
    async fn a_typed_call_sends_the_tables_name_and_params_and_reads_its_result() {
        let (client, mut from_client, to_client) = pair();
        let call = tokio::spawn(async move {
            let hello = HelloParams {
                name: "test".to_owned(),
                kind: ClientKind::Cli,
                version: "1.2.3".to_owned(),
                contract_version: 0,
            };
            (client.hello(&hello).await, client)
        });

        let (id, method, params) = sent(&mut from_client).await;
        assert_eq!(method, "server.hello");
        assert_eq!(
            params,
            json!({ "name": "test", "kind": "cli", "version": "1.2.3", "contractVersion": 0 })
        );
        let descriptor = json!({
            "serverId": "11111111-1111-1111-1111-111111111111",
            "name": "desk",
            "role": "hub",
            "contractVersion": 0,
            "capabilities": [],
            "limits": { "maxMessageBytes": 1024 },
            "sessionKind": "full",
        });
        to_client.send(result(id, descriptor)).await.unwrap();
        let (answer, _client) = call.await.unwrap();
        assert_eq!(answer.unwrap().server.name, "desk");
    }

    #[tokio::test]
    async fn answers_reach_the_calls_they_are_for_in_whatever_order_they_come() {
        let (client, mut from_client, to_client) = pair();
        let client = std::sync::Arc::new(client);
        let first = tokio::spawn({
            let client = client.clone();
            async move { client.call_raw("first", json!({})).await }
        });
        let (first_id, method, _) = sent(&mut from_client).await;
        assert_eq!(method, "first");
        let second = tokio::spawn({
            let client = client.clone();
            async move { client.call_raw("second", json!({})).await }
        });
        let (second_id, method, _) = sent(&mut from_client).await;
        assert_eq!(method, "second");
        assert_ne!(first_id, second_id);

        to_client
            .send(result(second_id, json!({ "for": "second" })))
            .await
            .unwrap();
        to_client
            .send(result(first_id, json!({ "for": "first" })))
            .await
            .unwrap();
        assert_eq!(first.await.unwrap().unwrap(), json!({ "for": "first" }));
        assert_eq!(second.await.unwrap().unwrap(), json!({ "for": "second" }));
    }

    #[tokio::test]
    async fn an_error_from_the_server_is_a_call_error_with_its_code() {
        let (client, mut from_client, to_client) = pair();
        let call = tokio::spawn(async move { (client.status().await, client) });
        let (id, _, _) = sent(&mut from_client).await;
        let refused = Frame::Response(Response {
            id: Some(RequestId::Number(id)),
            outcome: Err(RpcError::new(ErrorCode::Forbidden, "needs a full session")),
        });
        to_client.send(refused.encode()).await.unwrap();
        let (answer, _client) = call.await.unwrap();
        let Err(Error::Call(error)) = answer else {
            panic!("{answer:?}");
        };
        assert_eq!(error.kind(), ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn events_are_read_in_order_and_one_sent_before_a_response_is_there_after_it() {
        let (mut client, mut from_client, to_client) = pair();
        assert_eq!(client.try_next_event(), None);
        to_client.send(server_event(2)).await.unwrap();
        to_client.send(server_event(3)).await.unwrap();
        // A topic from a newer server, and a notification that is no event.
        to_client
            .send(
                json!({ "jsonrpc": "2.0", "method": "event", "params": { "topic": "weather" } })
                    .to_string(),
            )
            .await
            .unwrap();
        to_client
            .send(json!({ "jsonrpc": "2.0", "method": "later" }).to_string())
            .await
            .unwrap();

        let call = client.call_raw("servers.list", json!({}));
        let answer = async {
            let (id, _, _) = sent(&mut from_client).await;
            to_client.send(result(id, json!({}))).await.unwrap();
        };
        let (answered, ()) = tokio::join!(call, answer);
        answered.unwrap();

        let revisions: Vec<_> = std::iter::from_fn(|| client.try_next_event())
            .filter_map(|event| match event {
                Event::Servers(event) => Some(event.server.revision),
                _ => None,
            })
            .collect();
        assert_eq!(revisions, [2, 3]);
    }

    #[tokio::test]
    async fn taken_events_arrive_while_the_client_is_shared_and_end_with_the_connection() {
        let (mut client, mut from_client, to_client) = pair();
        let mut events = client.take_events();
        let client = std::sync::Arc::new(client);
        let call = tokio::spawn({
            let client = client.clone();
            async move { client.call_raw("first", json!({})).await }
        });
        let (id, _, _) = sent(&mut from_client).await;
        to_client.send(server_event(4)).await.unwrap();
        to_client.send(result(id, json!({}))).await.unwrap();
        assert!(matches!(events.recv().await, Some(Event::Servers(_))));
        assert!(call.await.unwrap().is_ok());
        drop(to_client);
        assert!(
            events.recv().await.is_none(),
            "the stream ends with the connection"
        );
    }

    #[tokio::test]
    async fn a_lost_connection_is_a_transport_error_for_the_call_waiting_and_the_next() {
        let (mut client, mut from_client, to_client) = pair();
        let call = client.call_raw("servers.list", json!({}));
        let lose = async {
            sent(&mut from_client).await;
            drop(to_client);
        };
        let (answer, ()) = tokio::join!(call, lose);
        assert!(matches!(answer, Err(Error::Transport(_))), "{answer:?}");

        let next = client.call_raw("servers.list", json!({})).await;
        assert!(matches!(next, Err(Error::Transport(_))), "{next:?}");
        assert_eq!(client.next_event().await, None);
    }

    #[tokio::test]
    async fn text_that_is_not_a_frame_ends_the_connection() {
        let (client, mut from_client, to_client) = pair();
        let call = client.call_raw("servers.list", json!({}));
        let garble = async {
            sent(&mut from_client).await;
            to_client.send("not a frame".to_owned()).await.unwrap();
        };
        let (answer, ()) = tokio::join!(call, garble);
        assert!(matches!(answer, Err(Error::Transport(_))), "{answer:?}");
    }

    #[tokio::test]
    async fn dropping_the_client_closes_its_end() {
        let (client, mut from_client, _to_client) = pair();
        drop(client);
        assert_eq!(from_client.recv().await, None);
    }
}
