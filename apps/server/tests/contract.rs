//! Contract tests: a real server inside the test process, on a data
//! directory of its own and a port the system picks, talked to through
//! `ritornel-client` as every program talks to it.
//!
//! Each test of a call runs twice: joined to the dispatcher by channels in
//! memory, and over a real socket on loopback. The frames are JSON on both,
//! and only the transport differs ("Transport-neutral dispatcher" in
//! docs/internals/contract.md).
//!
//! Nothing here sleeps. A test waits for an answer or for an event, and
//! every test is cut off, with a message, if it has not finished in
//! [`WAIT`].

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use ritornel_client::contract::methods::{
    EventsSubscribe, IdentityDryRun, IdentityReview, LibrarySetWatching, Method, ServerHello,
};
use ritornel_client::contract::{
    ART_SIZES, CONTRACT_VERSION, ClientKind, Descriptor, ErrorCode, Event, HelloParams, Id,
    JobState, JobTarget, LibraryStatus, Limits, ListSupport, PublicDescriptor, Receipt,
    ReceiptStatus, ServerInfo, ServerRole, SessionKind, SourceKind, Topic, WatchingParams,
    capability, route,
};
use ritornel_client::{Client, Credential, NotRunning, local};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, Error, HeldDir, Listen, Server};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// How long a test may take before it is failed. It is a bound, not a
/// wait: a test that passes never comes near it.
const WAIT: Duration = Duration::from_secs(60);

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
            "ritornel-server-contract-{}-{}",
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

/// What a test joins the client to the server with.
#[derive(Clone, Copy)]
enum Via {
    Memory,
    Socket,
}

/// Defines, for each test named, a module with the test run both ways.
macro_rules! on_both_transports {
    ($($test:ident)*) => {$(
        mod $test {
            use super::{Via, bounded};

            #[tokio::test]
            async fn in_memory() {
                bounded(super::$test(Via::Memory)).await;
            }

            #[tokio::test]
            async fn over_a_socket() {
                bounded(super::$test(Via::Socket)).await;
            }
        }
    )*};
}

async fn start_on(data_dir: &Path, listen: Listen) -> Result<Server, Error> {
    Server::start(
        HeldDir::take(data_dir.to_owned())?,
        listen,
        Arc::new(SystemClock),
        Arc::new(OsEntropy),
    )
    .await
}

/// A server running inside the test, on a directory of its own.
struct Running {
    server: Server,
    scratch: Scratch,
}

impl Running {
    /// Starts a server on a port the system picks.
    async fn start() -> Self {
        Self::start_with(Bounds::default()).await
    }

    /// Starts a server with bounds small enough for a test to reach.
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

    fn data_dir(&self) -> PathBuf {
        self.scratch.data_dir()
    }

    fn endpoint(&self) -> SocketAddr {
        self.server.endpoint()
    }

    /// The local credential, as a client on this machine reads it.
    fn credential(&self) -> Credential {
        Credential::new(self.token())
    }

    fn token(&self) -> String {
        std::fs::read_to_string(self.data_dir().join("credential"))
            .unwrap()
            .trim()
            .to_owned()
    }

    /// A client that has not said hello yet.
    async fn connect(&self, via: Via) -> Client {
        match via {
            Via::Memory => {
                let (outgoing, incoming) = self.server.connect_in_memory(SessionKind::Full);
                Client::over_memory(outgoing, incoming)
            }
            Via::Socket => Client::connect(self.endpoint(), &self.credential())
                .await
                .unwrap(),
        }
    }

    /// A client that has said hello.
    async fn greeted(&self, via: Via) -> Client {
        let client = self.connect(via).await;
        client.hello(&hello()).await.unwrap();
        client
    }

    /// What the server should say about itself to anyone.
    fn public_descriptor(&self) -> PublicDescriptor {
        PublicDescriptor {
            server_id: self.server.server_id(),
            name: "Ritornel server".to_owned(),
            role: ServerRole::Unassigned,
            contract_version: CONTRACT_VERSION,
            capabilities: [
                capability::EVENTS_SERVERS,
                capability::EVENTS_LIBRARY,
                capability::LISTS,
                capability::MEDIA_ART,
            ]
            .map(str::to_owned)
            .to_vec(),
            limits: Limits {
                max_message_bytes: 1 << 20,
                max_views: 16,
                max_window_rows: 500,
                view_idle_ms: 600_000,
            },
            lists: ListSupport::all(),
            art_sizes: ART_SIZES.to_vec(),
        }
    }

    /// This server's row of `servers.list`, with this name and revision.
    fn info(&self, name: &str, revision: u64) -> ServerInfo {
        ServerInfo {
            server_id: self.server.server_id(),
            name: name.to_owned(),
            role: ServerRole::Unassigned,
            is_this_server: true,
            reachable: true,
            revision,
        }
    }
}

fn hello() -> HelloParams {
    HelloParams {
        name: "contract-test".to_owned(),
        kind: ClientKind::Cli,
        version: "1.2.3".to_owned(),
        contract_version: CONTRACT_VERSION,
    }
}

/// The contract's code and the details of the error a call was answered
/// with.
fn refused<T: std::fmt::Debug>(
    answer: Result<T, ritornel_client::Error>,
) -> (ErrorCode, bool, Value) {
    match answer {
        Err(ritornel_client::Error::Call(error)) => (
            error.data.code,
            error.data.retryable,
            Value::Object(error.data.details),
        ),
        other => panic!("expected an error from the server, and got {other:?}"),
    }
}

on_both_transports! {
    hello_answers_with_the_descriptor
    the_status_reports_versions_role_and_server_id
    the_list_of_servers_is_this_server
    creating_a_library_makes_the_server_its_hub
    a_folder_is_added_as_a_source_once_and_only_where_there_is_one
    a_scan_runs_as_a_job_and_the_stats_say_what_came_out
    a_job_can_be_asked_to_stop_and_one_there_is_not_is_not_found
    the_dry_run_and_the_review_read_the_library_and_check_what_they_are_asked
    a_subscriber_is_sent_a_servers_event_and_after_unsubscribing_none
    changes_a_client_has_not_read_yet_reach_it_as_one_event
    a_method_the_server_does_not_have_is_unsupported
    params_of_the_wrong_shape_are_invalid
    a_call_before_hello_is_refused
    a_client_built_for_another_contract_version_is_refused
    a_batch_is_refused_and_a_notification_gets_no_answer
    stopping_the_server_closes_its_connections_and_then_its_database
    a_message_over_the_limit_closes_the_connection_and_no_other
    watching_is_turned_off_and_on_and_the_status_says_so
}

async fn watching_is_turned_off_and_on_and_the_status_says_so(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    client
        .create_library(Id::from_bytes([1; 16]))
        .await
        .unwrap();
    // On until turned off, though this run of the server watches nothing.
    let before = client.status().await.unwrap().watch.unwrap();
    assert!(before.enabled && !before.on);
    let set = |key: u8, on: bool| WatchingParams {
        idempotency_key: Id::from_bytes([key; 16]),
        on,
    };
    let off = client
        .call::<LibrarySetWatching>(&set(2, false))
        .await
        .unwrap();
    assert_eq!(
        off.receipt,
        Receipt {
            command_id: Id::from_bytes([2; 16]),
            status: ReceiptStatus::Done,
            revision: None,
        }
    );
    assert!(!off.watch.enabled && !off.watch.on);
    assert_eq!(client.status().await.unwrap().watch.unwrap(), off.watch);
    // Sent again, it answers the same.
    assert_eq!(
        client
            .call::<LibrarySetWatching>(&set(2, false))
            .await
            .unwrap(),
        off
    );
    let on = client
        .call::<LibrarySetWatching>(&set(3, true))
        .await
        .unwrap();
    assert!(on.watch.enabled);
    assert_eq!(
        on.watch.off.as_deref(),
        Some("this run of the server does not watch")
    );
}

async fn hello_answers_with_the_descriptor(via: Via) {
    let running = Running::start().await;
    let client = running.connect(via).await;
    assert_eq!(
        client.hello(&hello()).await.unwrap(),
        Descriptor {
            server: running.public_descriptor(),
            session_kind: SessionKind::Full,
        }
    );
    // The same descriptor, less the session, is served to anyone.
    assert_eq!(
        ritornel_client::descriptor(running.endpoint())
            .await
            .unwrap(),
        running.public_descriptor()
    );
    // Nothing that does not exist yet is in it.
    let raw = client
        .call_raw("server.hello", serde_json::to_value(hello()).unwrap())
        .await
        .unwrap();
    let mut fields: Vec<_> = raw
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "artSizes",
            "capabilities",
            "contractVersion",
            "limits",
            "lists",
            "name",
            "role",
            "serverId",
            "sessionKind"
        ]
    );
}

async fn the_status_reports_versions_role_and_server_id(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let status = client.status().await.unwrap();
    assert_eq!(status.server_id, running.server.server_id());
    assert_eq!(status.name, "Ritornel server");
    assert_eq!(status.role, ServerRole::Unassigned);
    assert_eq!(status.versions.server, env!("CARGO_PKG_VERSION"));
    assert_eq!(status.versions.contract, CONTRACT_VERSION);
    assert_eq!(status.versions.sqlite, "3.54.1");
    assert_eq!(status.versions.server_schema, 1);
    // It has no library, and says nothing of one.
    assert_eq!(status.versions.library_schema, None);
    assert_eq!(status.library, None);
    assert!(status.free_disk.data_bytes.unwrap() > 0);
    assert_eq!(Path::new(&status.log_path), running.data_dir().join("logs"));
}

async fn creating_a_library_makes_the_server_its_hub(via: Via) {
    let running = Running::start().await;
    let mut client = running.greeted(via).await;
    client.subscribe(&[Topic::Servers]).await.unwrap();

    let key = ritornel_client::idempotency_key().unwrap();
    let created = client.create_library(key).await.unwrap();
    assert_eq!(
        created.receipt,
        Receipt {
            command_id: key,
            status: ReceiptStatus::Done,
            revision: Some(1),
        }
    );
    assert!(running.data_dir().join("library.db").exists());

    // The status says what now exists, and everything that names the
    // server's role says hub: the event, the list and the descriptor.
    let status = client.status().await.unwrap();
    assert_eq!(status.role, ServerRole::Hub);
    assert_eq!(
        status.versions.library_schema,
        Some(ritornel_engine::LIBRARY_SCHEMA_VERSION)
    );
    assert_eq!(
        status.library,
        Some(LibraryStatus {
            library_id: created.library_id,
            revision: 1,
        })
    );
    let hub = ServerInfo {
        role: ServerRole::Hub,
        ..running.info("Ritornel server", 2)
    };
    let Some(Event::Servers(event)) = client.next_event().await else {
        panic!("the server's new status was not sent");
    };
    assert_eq!(event.server, hub);
    assert_eq!(client.servers().await.unwrap().servers, [hub]);
    assert_eq!(
        ritornel_client::descriptor(running.endpoint())
            .await
            .unwrap()
            .role,
        ServerRole::Hub
    );

    // The same command again is answered as it was. Another is refused: a
    // server holds one library.
    assert_eq!(client.create_library(key).await.unwrap(), created);
    let another = ritornel_client::idempotency_key().unwrap();
    assert_eq!(
        refused(client.create_library(another).await),
        (
            ErrorCode::Conflict,
            false,
            json!({ "libraryId": created.library_id.to_string() })
        )
    );
    assert_eq!(client.status().await.unwrap().library, status.library);
    assert!(client.try_next_event().is_none());
}

/// A folder of the audio fixtures under the test's own directory: three
/// files that are audio, and one that is not.
fn music(running: &Running) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
    let folder = running.scratch.0.join("music");
    for (to, fixture) in [
        ("Tin Weather/02.flac", "a.flac"),
        ("Tin Weather/copy.mp3", "a.mp3"),
        ("loose.ogg", "a.retagged.ogg"),
        ("broken.flac", "not-audio.flac"),
    ] {
        let to = folder.join(to);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixtures.join(fixture), to).unwrap();
    }
    folder
}

fn key() -> Id {
    ritornel_client::idempotency_key().unwrap()
}

async fn a_folder_is_added_as_a_source_once_and_only_where_there_is_one(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let folder = music(&running);
    let path = folder.to_str().unwrap();

    // With no library there is nothing to add it to, or to count.
    let (code, ..) = refused(client.sources().await);
    assert_eq!(code, ErrorCode::Unavailable);
    let (code, ..) = refused(client.add_source(key(), "main", path).await);
    assert_eq!(code, ErrorCode::Unavailable);
    let (code, ..) = refused(client.library_stats().await);
    assert_eq!(code, ErrorCode::Unavailable);
    client.create_library(key()).await.unwrap();
    assert_eq!(client.sources().await.unwrap().sources, []);

    for (name, at, field) in [
        ("", path, "name"),
        ("a/b", path, "name"),
        ("main", "music", "path"),
        ("main", "/there/is/no/such/folder", "path"),
        ("main", folder.join("loose.ogg").to_str().unwrap(), "path"),
    ] {
        let (code, _, details) = refused(client.add_source(key(), name, at).await);
        assert_eq!(
            (code, details),
            (ErrorCode::Invalid, json!({ "field": field })),
            "{name} {at}"
        );
    }

    let first = key();
    let added = client.add_source(first, "main", path).await.unwrap();
    assert_eq!(
        added.receipt,
        Receipt {
            command_id: first,
            status: ReceiptStatus::Done,
            revision: Some(2),
        }
    );
    assert_eq!(
        (
            added.source.name.as_str(),
            added.source.kind,
            added.source.path.as_deref()
        ),
        ("main", SourceKind::Folder, Some(path))
    );
    assert_eq!(
        client.sources().await.unwrap().sources,
        std::slice::from_ref(&added.source)
    );
    // The command sent again is answered as it was, and adds nothing. With
    // other params its key is refused, and another command for the same
    // name is told which source is in its way.
    assert_eq!(client.add_source(first, "main", path).await.unwrap(), added);
    let (code, ..) = refused(client.add_source(first, "other", path).await);
    assert_eq!(code, ErrorCode::IdempotencyMismatch);
    let (code, _, details) = refused(client.add_source(key(), "main", path).await);
    assert_eq!(
        (code, details),
        (
            ErrorCode::Conflict,
            json!({ "sourceId": added.source.source_id.to_string() })
        )
    );
    assert_eq!(client.sources().await.unwrap().sources.len(), 1);
    assert_eq!(client.status().await.unwrap().library.unwrap().revision, 2);
    // Nothing was written under the folder.
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 3);
}

/// Reads events until the job has ended, and returns it as it ended with
/// the library revisions the `library` topic named on the way.
async fn ended(client: &mut Client, job_id: Id) -> (ritornel_client::contract::Job, Vec<u64>) {
    let mut revisions = Vec::new();
    loop {
        match client.next_event().await.unwrap() {
            Event::Jobs(event) if event.job.job_id == job_id && event.job.state.ended() => {
                return (event.job, revisions);
            }
            Event::Library(event) => revisions.push(event.library_revision),
            _ => {}
        }
    }
}

/// Reads events until the `library` topic has said `revision`. An unsent
/// event keeps the place of the first it was merged from, so a job's end
/// can come before the library's last change when the session sends late.
async fn said(client: &mut Client, mut revisions: Vec<u64>, revision: u64) -> Vec<u64> {
    while revisions.last() < Some(&revision) {
        if let Event::Library(event) = client.next_event().await.unwrap() {
            revisions.push(event.library_revision);
        }
    }
    revisions
}

async fn a_scan_runs_as_a_job_and_the_stats_say_what_came_out(via: Via) {
    let running = Running::start().await;
    let mut client = running.greeted(via).await;
    let folder = music(&running);
    client.create_library(key()).await.unwrap();
    let source = client
        .add_source(key(), "main", folder.to_str().unwrap())
        .await
        .unwrap()
        .source;
    client
        .subscribe(&[Topic::Jobs, Topic::Library])
        .await
        .unwrap();

    let (code, ..) = refused(client.start_scan(key(), Id::from_bytes([9; 16])).await);
    assert_eq!(code, ErrorCode::NotFound);
    let first = key();
    let started = client.start_scan(first, source.source_id).await.unwrap();
    assert_eq!(
        started.receipt,
        Receipt {
            command_id: first,
            status: ReceiptStatus::Accepted,
            revision: None,
        }
    );
    assert_eq!(started.job.kind, "scan");
    assert_eq!(
        started.job.target,
        Some(JobTarget::Source {
            id: source.source_id
        })
    );
    // The command sent again names the same job, whatever has become of it.
    assert_eq!(
        client
            .start_scan(first, source.source_id)
            .await
            .unwrap()
            .job
            .job_id,
        started.job.job_id
    );

    let (job, revisions) = ended(&mut client, started.job.job_id).await;
    assert_eq!(job.state, JobState::Succeeded);
    let scan = job.scan.unwrap();
    assert_eq!((scan.files, scan.new, scan.unreadable), (4, 4, 1));
    assert_eq!(
        (scan.assignments, scan.recordings, scan.releases),
        (3, 2, 1)
    );
    assert_eq!(
        (job.done, job.total, job.failed, job.skipped),
        (4, Some(4), 1, 0)
    );
    // The files were recorded at revision 3 and resolved at 4, and the
    // library topic said so.
    assert_eq!(said(&mut client, revisions, 4).await.last(), Some(&4));
    assert_eq!(client.status().await.unwrap().library.unwrap().revision, 4);

    // The job is a durable resource: it is listed and can be read after it
    // has ended, with what could not be read.
    let listed = client.jobs().await.unwrap().jobs;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], job);
    let detail = client.job(job.job_id).await.unwrap();
    assert_eq!(detail.job, job);
    assert_eq!(detail.failed_items.len(), 1);
    assert_eq!(detail.failed_items[0].item, "broken.flac");
    assert!(detail.failed_items[0].detail.contains("unsupported"));

    let stats = client.library_stats().await.unwrap();
    assert_eq!(
        (stats.files, stats.files_unreadable, stats.library_revision),
        (4, 1, 4)
    );
    assert_eq!(
        (
            stats.recordings,
            stats.releases,
            stats.release_groups,
            stats.artists
        ),
        (2, 1, 1, 2)
    );
    assert_eq!(
        (stats.appearances, stats.loose_tracks, stats.needs_review),
        (3, 1, 0)
    );
    assert_eq!(
        (
            stats.groups_with_kind,
            stats.groups_with_first_year_from_tag
        ),
        (1, 1)
    );

    // A second scan finds nothing changed, writes nothing, and leaves the
    // library at the revision it was at.
    let again = client.start_scan(key(), source.source_id).await.unwrap();
    assert_ne!(again.job.job_id, job.job_id);
    let (quiet, revisions) = ended(&mut client, again.job.job_id).await;
    let scan = quiet.scan.unwrap();
    assert_eq!(
        (quiet.state, scan.files, scan.unchanged, scan.new),
        (JobState::Succeeded, 4, 4, 0)
    );
    assert_eq!(revisions, [] as [u64; 0]);
    assert_eq!(client.status().await.unwrap().library.unwrap().revision, 4);
    assert_eq!(client.library_stats().await.unwrap(), stats);
    assert_eq!(client.jobs().await.unwrap().jobs.len(), 2);
}

async fn a_job_can_be_asked_to_stop_and_one_there_is_not_is_not_found(via: Via) {
    let running = Running::start().await;
    let mut client = running.greeted(via).await;
    let folder = music(&running);
    client.create_library(key()).await.unwrap();
    let source = client
        .add_source(key(), "main", folder.to_str().unwrap())
        .await
        .unwrap()
        .source;
    client.subscribe(&[Topic::Jobs]).await.unwrap();

    let (code, ..) = refused(client.job(Id::from_bytes([9; 16])).await);
    assert_eq!(code, ErrorCode::NotFound);
    let (code, ..) = refused(client.cancel_job(key(), Id::from_bytes([9; 16])).await);
    assert_eq!(code, ErrorCode::NotFound);

    // The scan is asked to stop the moment it is started. It may have
    // ended by then, or stop at its next step: either way it ends, and
    // what it committed stays.
    let started = client.start_scan(key(), source.source_id).await.unwrap();
    let cancel = key();
    let asked = client.cancel_job(cancel, started.job.job_id).await.unwrap();
    assert_eq!(asked.receipt.command_id, cancel);
    let (job, _) = ended(&mut client, started.job.job_id).await;
    assert!(
        matches!(job.state, JobState::Cancelled | JobState::Succeeded),
        "{:?}",
        job.state
    );
    // Asking again changes nothing.
    let again = client.cancel_job(cancel, job.job_id).await.unwrap();
    assert_eq!(
        (again.job.state, again.receipt.status),
        (job.state, ReceiptStatus::Done)
    );
    // Whatever was left, the next scan finishes.
    let next = client.start_scan(key(), source.source_id).await.unwrap();
    let (job, _) = ended(&mut client, next.job.job_id).await;
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(client.library_stats().await.unwrap().files, 4);
}

async fn the_dry_run_and_the_review_read_the_library_and_check_what_they_are_asked(via: Via) {
    let running = Running::start().await;
    let mut client = running.greeted(via).await;
    let (code, ..) = refused(client.call_raw(IdentityDryRun::NAME, json!({})).await);
    assert_eq!(code, ErrorCode::Unavailable);
    client.create_library(key()).await.unwrap();
    let folder = music(&running);
    let source = client
        .add_source(key(), "main", folder.to_str().unwrap())
        .await
        .unwrap()
        .source;
    client.subscribe(&[Topic::Jobs]).await.unwrap();
    let started = client.start_scan(key(), source.source_id).await.unwrap();
    let (job, _) = ended(&mut client, started.job.job_id).await;
    assert_eq!(job.state, JobState::Succeeded);

    let run = client
        .call_raw(IdentityDryRun::NAME, json!({ "detail": true }))
        .await
        .unwrap();
    assert_eq!(
        (&run["items"], &run["wouldChange"], &run["wouldWrite"]),
        (&json!(4), &json!(0), &json!(false))
    );
    let review = client
        .call::<IdentityReview>(&Default::default())
        .await
        .unwrap();
    assert_eq!(review.reviews, []);
    for (params, field) in [
        (json!({ "policy": { "bindMax": 1.5 } }), "bindMax"),
        (json!({ "policy": { "minGap": -0.1 } }), "minGap"),
        (json!({ "policy": { "reviewBelow": 2 } }), "reviewBelow"),
        (json!({ "policy": { "version": 4 } }), "version"),
        (json!({ "policy": { "markerList": 0 } }), "markerList"),
        (
            json!({ "policy": { "acrossAlbumsWithinMs": 60_001 } }),
            "acrossAlbumsWithinMs",
        ),
        (
            json!({ "policy": { "durationGraceMs": 60_001 } }),
            "durationGraceMs",
        ),
    ] {
        let (code, retryable, details) =
            refused(client.call_raw(IdentityDryRun::NAME, params).await);
        assert_eq!((code, retryable), (ErrorCode::Invalid, false));
        assert_eq!(details["field"], field);
    }
}

async fn the_list_of_servers_is_this_server(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    assert_eq!(
        client.servers().await.unwrap().servers,
        [running.info("Ritornel server", 1)]
    );
}

async fn a_subscriber_is_sent_a_servers_event_and_after_unsubscribing_none(via: Via) {
    let running = Running::start().await;
    let mut subscriber = running.greeted(via).await;
    let mut other = running.greeted(via).await;
    assert_eq!(
        subscriber
            .subscribe(&[Topic::Servers])
            .await
            .unwrap()
            .topics,
        [Topic::Servers]
    );

    // The one thing that changes a server's status in this milestone.
    running.server.rename("attic").unwrap();
    let Some(Event::Servers(event)) = subscriber.next_event().await else {
        panic!("the connection closed");
    };
    // The event is the server's full status, not what changed in it.
    assert_eq!(event.server, running.info("attic", 2));
    // What a call reads afterwards agrees with it.
    assert_eq!(
        subscriber.servers().await.unwrap().servers,
        std::slice::from_ref(&event.server)
    );
    assert_eq!(subscriber.try_next_event(), None);

    // A session that did not subscribe is sent nothing. Frames arrive in
    // order, so an event would have been here before this answer.
    assert_eq!(other.servers().await.unwrap().servers, [event.server]);
    assert_eq!(other.try_next_event(), None);

    assert_eq!(
        subscriber
            .unsubscribe(&[Topic::Servers])
            .await
            .unwrap()
            .topics,
        []
    );
    running.server.rename("cellar").unwrap();
    assert_eq!(
        subscriber.servers().await.unwrap().servers,
        [running.info("cellar", 3)]
    );
    assert_eq!(subscriber.try_next_event(), None);
}

async fn changes_a_client_has_not_read_yet_reach_it_as_one_event(via: Via) {
    let running = Running::start().await;
    let mut subscriber = running.greeted(via).await;
    subscriber.subscribe(&[Topic::Servers]).await.unwrap();

    // Two changes with nothing sent between them: the test's runtime has
    // one thread, and this does not let go of it.
    running.server.rename("attic").unwrap();
    running.server.rename("cellar").unwrap();

    let Some(Event::Servers(event)) = subscriber.next_event().await else {
        panic!("the connection closed");
    };
    assert_eq!(event.server, running.info("cellar", 3));
    subscriber.servers().await.unwrap();
    assert_eq!(subscriber.try_next_event(), None);
}

async fn a_method_the_server_does_not_have_is_unsupported(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    assert_eq!(
        refused(client.call_raw("weather.get", json!({})).await),
        (ErrorCode::Unsupported, false, json!({}))
    );
    // The connection is as good as it was.
    client.servers().await.unwrap();
}

async fn params_of_the_wrong_shape_are_invalid(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    for (method, params) in [
        (ServerHello::NAME, json!({ "name": "contract-test" })),
        (EventsSubscribe::NAME, json!({ "topics": "servers" })),
        (EventsSubscribe::NAME, json!({ "topics": ["weather"] })),
        (EventsSubscribe::NAME, json!(["servers"])),
        ("server.status", json!(7)),
    ] {
        let (code, retryable, _) = refused(client.call_raw(method, params.clone()).await);
        assert_eq!(
            (code, retryable),
            (ErrorCode::Invalid, false),
            "{method} {params}"
        );
    }
    client.servers().await.unwrap();
}

async fn a_call_before_hello_is_refused(via: Via) {
    let running = Running::start().await;
    let client = running.connect(via).await;
    assert_eq!(
        refused(client.servers().await),
        (
            ErrorCode::Invalid,
            false,
            json!({ "reason": "hello_required" })
        )
    );
    assert_eq!(
        refused(client.subscribe(&[Topic::Servers]).await).0,
        ErrorCode::Invalid
    );
    // After hello the same connection takes the same call.
    client.hello(&hello()).await.unwrap();
    client.servers().await.unwrap();
}

async fn a_client_built_for_another_contract_version_is_refused(via: Via) {
    let running = Running::start().await;
    let client = running.connect(via).await;
    let other = HelloParams {
        contract_version: CONTRACT_VERSION + 1,
        ..hello()
    };
    assert_eq!(
        refused(client.hello(&other).await),
        (
            ErrorCode::Unsupported,
            false,
            json!({ "contractVersion": CONTRACT_VERSION })
        )
    );
    assert_eq!(refused(client.servers().await).0, ErrorCode::Invalid);
}

/// A connection that carries whatever text a test gives it, for the frames
/// a client never sends.
enum Raw {
    Memory(Sender<String>, Receiver<String>),
    Socket(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

impl Raw {
    async fn open(running: &Running, via: Via) -> Self {
        match via {
            Via::Memory => {
                let (outgoing, incoming) = running.server.connect_in_memory(SessionKind::Full);
                Self::Memory(outgoing, incoming)
            }
            Via::Socket => {
                let authorization = format!("Bearer {}", running.token());
                let socket = upgrade(running.endpoint(), &[("authorization", &authorization)])
                    .await
                    .unwrap();
                Self::Socket(Box::new(socket))
            }
        }
    }

    async fn send(&mut self, text: &str) {
        match self {
            Self::Memory(outgoing, _) => outgoing.send(text.to_owned()).await.unwrap(),
            Self::Socket(socket) => socket.send(Message::text(text)).await.unwrap(),
        }
    }

    /// Sends text the server may hang up on before it has all arrived.
    async fn send_if_open(&mut self, text: &str) {
        match self {
            Self::Memory(outgoing, _) => {
                let _ = outgoing.send(text.to_owned()).await;
            }
            Self::Socket(socket) => {
                let _ = socket.send(Message::text(text)).await;
            }
        }
    }

    /// The next frame, as JSON. `None` when the server has closed.
    async fn receive(&mut self) -> Option<Value> {
        let text = match self {
            Self::Memory(_, incoming) => incoming.recv().await?,
            Self::Socket(socket) => match socket.next().await? {
                Ok(Message::Text(text)) => text.as_str().to_owned(),
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(other) => panic!("the server sent {other:?}"),
            },
        };
        Some(serde_json::from_str(&text).unwrap())
    }
}

async fn a_batch_is_refused_and_a_notification_gets_no_answer(via: Via) {
    let running = Running::start().await;
    let mut raw = Raw::open(&running, via).await;

    raw.send(r#"[{"jsonrpc":"2.0","id":1,"method":"servers.list"}]"#)
        .await;
    assert_eq!(
        raw.receive().await.unwrap(),
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {
                "code": -32600,
                "message": "batches are not supported",
                "data": { "code": "invalid", "retryable": false, "details": {} },
            },
        })
    );

    raw.send("{ this is not json").await;
    let answer = raw.receive().await.unwrap();
    assert_eq!(answer["id"], Value::Null);
    assert_eq!(answer["error"]["code"], -32700);
    assert_eq!(answer["error"]["data"]["code"], "invalid");

    // A notification is not answered, even when it is wrong. The next
    // answer on the connection is the one to the request after it.
    raw.send(r#"{"jsonrpc":"2.0","method":"servers.list"}"#)
        .await;
    raw.send(r#"{"jsonrpc":"2.0","id":"after","method":"servers.list"}"#)
        .await;
    let answer = raw.receive().await.unwrap();
    assert_eq!(answer["id"], "after");
    assert_eq!(
        answer["error"]["data"]["details"]["reason"],
        "hello_required"
    );
}

async fn stopping_the_server_closes_its_connections_and_then_its_database(via: Via) {
    let running = Running::start().await;
    let mut client = running.greeted(via).await;
    let data_dir = running.data_dir();
    assert!(data_dir.join("server.json").exists());

    let Running { server, scratch } = running;
    server.stop().await.unwrap();

    // The connection was closed from the server's side.
    assert_eq!(client.next_event().await, None);
    let after = client.servers().await;
    assert!(
        matches!(after, Err(ritornel_client::Error::Transport(_))),
        "{after:?}"
    );
    // The database was closed after it: its write-ahead log is emptied,
    // and SQLite removes an empty one.
    assert!(!data_dir.join("server.db-wal").exists());
    assert!(!data_dir.join("server.json").exists());
    // And the directory is free for the next server.
    drop(HeldDir::take(data_dir).unwrap());
    drop(scratch);
}

/// The limit is the dispatcher's, so it is the same on every transport: a
/// frame of exactly the limit is read, and one byte more closes the
/// connection with no answer.
async fn a_message_over_the_limit_closes_the_connection_and_no_other(via: Via) {
    let running = Running::start().await;
    let limit = usize::try_from(running.public_descriptor().limits.max_message_bytes).unwrap();
    let bystander = running.greeted(via).await;
    // Valid JSON, so only its length can be what refuses it.
    let frame = |bytes: usize| {
        let padding = bytes - r#"{"jsonrpc":"2.0","id":1,"method":""}"#.len();
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"{}"}}"#,
            "x".repeat(padding)
        )
    };

    let mut raw = Raw::open(&running, via).await;
    raw.send(&frame(limit)).await;
    assert_eq!(
        raw.receive().await.unwrap()["error"]["data"]["code"],
        "unsupported"
    );

    raw.send_if_open(&frame(limit + 1)).await;
    assert_eq!(raw.receive().await, None);

    bystander.servers().await.unwrap();
}

/// Tries to open the socket with these headers. `Err` is the HTTP status
/// the server refused with.
async fn upgrade(
    endpoint: SocketAddr,
    headers: &[(&'static str, &str)],
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, u16> {
    let mut request = format!("ws://{endpoint}{}", route::WS)
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

/// The status of a `GET`, with a credential or without.
async fn status_of(endpoint: SocketAddr, path: &str, credential: Option<&Credential>) -> u16 {
    ritornel_client::get(endpoint, path, credential)
        .await
        .unwrap()
        .status
}

#[tokio::test]
async fn a_limited_session_may_not_ask_for_the_status_or_change_anything() {
    bounded(async {
        let running = Running::start().await;
        let (outgoing, incoming) = running.server.connect_in_memory(SessionKind::Limited);
        let client = Client::over_memory(outgoing, incoming);
        assert_eq!(
            client.hello(&hello()).await.unwrap().session_kind,
            SessionKind::Limited
        );
        let (code, retryable, _) = refused(client.status().await);
        assert_eq!((code, retryable), (ErrorCode::Forbidden, false));
        let (code, retryable, _) = refused(client.create_library(key()).await);
        assert_eq!((code, retryable), (ErrorCode::Forbidden, false));
        assert!(!running.data_dir().join("library.db").exists());
        for forbidden in [
            refused(client.sources().await),
            refused(client.add_source(key(), "main", "/srv/invented").await),
            refused(client.start_scan(key(), key()).await),
            refused(client.jobs().await),
            refused(client.subscribe(&[Topic::Servers, Topic::Jobs]).await),
            refused(client.job(key()).await),
            refused(client.cancel_job(key(), key()).await),
            refused(client.call::<IdentityDryRun>(&Default::default()).await),
            refused(client.call::<IdentityReview>(&Default::default()).await),
        ] {
            assert_eq!((forbidden.0, forbidden.1), (ErrorCode::Forbidden, false));
        }
        // Everything else so far is open to any session: reading the
        // library is, though there is none here to read.
        client.servers().await.unwrap();
        client
            .subscribe(&[Topic::Servers, Topic::Library])
            .await
            .unwrap();
        let (code, ..) = refused(client.library_stats().await);
        assert_eq!(code, ErrorCode::Unavailable);
    })
    .await;
}

#[tokio::test]
async fn a_socket_upgrade_needs_the_local_credential() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();
        let token = running.token();

        assert_eq!(upgrade(endpoint, &[]).await.err(), Some(401));
        for wrong in [
            "Bearer".to_owned(),
            "Bearer ".to_owned(),
            format!("Bearer {}", "0".repeat(64)),
            format!("Bearer {token}0"),
            format!("Basic {token}"),
            token.clone(),
        ] {
            assert_eq!(
                upgrade(endpoint, &[("authorization", &wrong)]).await.err(),
                Some(401),
                "an Authorization of {} characters",
                wrong.len()
            );
        }
        // The client reports the same refusal.
        for wrong in ["0".repeat(64), "not\na token".to_owned(), String::new()] {
            let refused = Client::connect(endpoint, &Credential::new(&wrong)).await;
            assert!(
                matches!(
                    refused,
                    Err(ritornel_client::Error::Refused { status: 401 })
                ),
                "{refused:?}"
            );
        }

        // The credential in the file opens it, however the scheme is
        // written.
        for scheme in ["Bearer", "bearer"] {
            let authorization = format!("{scheme} {token}");
            upgrade(endpoint, &[("authorization", &authorization)])
                .await
                .unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn a_socket_upgrade_with_an_origin_is_refused() {
    bounded(async {
        let running = Running::start().await;
        let authorization = format!("Bearer {}", running.token());
        // A page cannot send the header, so with the header every origin
        // is a stranger: one on the web, one on this machine, and the
        // desktop's own, which is let in only with a ticket
        // (`tests/ticket.rs`).
        for origin in [
            "https://example.com",
            "http://127.0.0.1:4747",
            "http://localhost",
            "tauri://localhost",
            "null",
        ] {
            assert_eq!(
                upgrade(
                    running.endpoint(),
                    &[("authorization", &authorization), ("origin", origin)]
                )
                .await
                .err(),
                Some(403),
                "{origin}"
            );
        }
        // An origin does not stand in for a credential either.
        assert_eq!(
            upgrade(running.endpoint(), &[("origin", "https://example.com")])
                .await
                .err(),
            Some(401)
        );
    })
    .await;
}

#[tokio::test]
async fn the_descriptor_is_the_only_route_that_needs_no_credential() {
    bounded(async {
        let running = Running::start().await;
        let endpoint = running.endpoint();
        let credential = running.credential();
        let wrong = Credential::new("0".repeat(64));

        let open = ritornel_client::get(endpoint, route::DESCRIPTOR, None)
            .await
            .unwrap();
        assert_eq!(open.status, 200);
        assert_eq!(
            serde_json::from_slice::<PublicDescriptor>(&open.body).unwrap(),
            running.public_descriptor()
        );
        // Nothing in it is a secret.
        assert!(
            !String::from_utf8(open.body)
                .unwrap()
                .contains(&running.token())
        );

        // Health, the socket and a path that does not exist all answer a
        // stranger the same way.
        for path in [
            route::HEALTH,
            route::WS,
            "/v1/nothing",
            "/",
            "/v1/descriptor/x",
        ] {
            assert_eq!(status_of(endpoint, path, None).await, 401, "{path}");
            assert_eq!(status_of(endpoint, path, Some(&wrong)).await, 401, "{path}");
        }
        assert_eq!(
            serde_json::to_value(
                ritornel_client::health(endpoint, &credential)
                    .await
                    .unwrap()
            )
            .unwrap(),
            json!({ "status": "ok" })
        );
        assert!(matches!(
            ritornel_client::health(endpoint, &wrong).await,
            Err(ritornel_client::Error::Refused { status: 401 })
        ));
        assert_eq!(
            status_of(endpoint, "/v1/nothing", Some(&credential)).await,
            404
        );
        // The socket's route without an upgrade is not a way in.
        let plain = status_of(endpoint, route::WS, Some(&credential)).await;
        assert!((400..500).contains(&plain), "{plain}");
    })
    .await;
}

/// Everything the server sends on a connection until it closes it.
async fn read_until_closed(connection: &mut TcpStream) -> String {
    let mut sent = Vec::new();
    // A connection the server cut off may end in a reset, which is closed
    // too.
    let _ = connection.read_to_end(&mut sent).await;
    String::from_utf8_lossy(&sent).into_owned()
}

#[tokio::test]
async fn a_connection_that_sends_no_whole_request_is_closed_when_its_time_is_up() {
    bounded(async {
        let header_wait = Duration::from_millis(250);
        let running = Running::start_with(Bounds {
            header_wait,
            ..Bounds::default()
        })
        .await;

        // Half a request line, nothing at all, and a whole request with
        // nothing after it. No credential is involved in any of them.
        for (sent, answered) in [
            ("GET /v1/descr", false),
            ("", false),
            ("GET /v1/descriptor HTTP/1.1\r\nHost: test\r\n\r\n", true),
        ] {
            let mut connection = TcpStream::connect(running.endpoint()).await.unwrap();
            let opened = Instant::now();
            connection.write_all(sent.as_bytes()).await.unwrap();

            // Nothing more is sent, and the server ends it by itself.
            let received = read_until_closed(&mut connection).await;
            assert!(
                opened.elapsed() >= header_wait / 2,
                "{sent:?} was closed after {:?}, before its time was up",
                opened.elapsed()
            );
            assert_eq!(
                received.starts_with("HTTP/1.1 200 OK"),
                answered,
                "{sent:?}: {received:?}"
            );
        }

        // The server is as it was.
        ritornel_client::descriptor(running.endpoint())
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn with_every_place_taken_a_connection_waits_for_one_and_is_then_served() {
    bounded(async {
        let header_wait = Duration::from_millis(300);
        let running = Running::start_with(Bounds {
            max_connections: 2,
            header_wait,
            ..Bounds::default()
        })
        .await;

        // Two connections that say nothing take both places.
        let mut silent = Vec::new();
        for _ in 0..2 {
            silent.push(TcpStream::connect(running.endpoint()).await.unwrap());
        }
        let taken = Instant::now();

        // The next one is not served until a place is free, and a place
        // comes free because the silent ones run out of time. The public
        // descriptor is not starved for ever.
        let descriptor = ritornel_client::descriptor(running.endpoint())
            .await
            .unwrap();
        assert_eq!(descriptor, running.public_descriptor());
        assert!(
            taken.elapsed() >= header_wait / 2,
            "it was served after {:?}, while both places were taken",
            taken.elapsed()
        );

        // The server closed the silent ones, and serves as before.
        for connection in &mut silent {
            assert_eq!(read_until_closed(connection).await, "");
        }
        let client = running.greeted(Via::Socket).await;
        client.servers().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn no_more_sessions_open_than_the_server_takes() {
    bounded(async {
        let running = Running::start_with(Bounds {
            max_sessions: 2,
            ..Bounds::default()
        })
        .await;
        let authorization = format!("Bearer {}", running.token());
        let first = running.greeted(Via::Socket).await;
        let second = running.greeted(Via::Memory).await;

        // A third is refused on either transport, and the two go on.
        assert_eq!(
            upgrade(running.endpoint(), &[("authorization", &authorization)])
                .await
                .err(),
            Some(503)
        );
        let (outgoing, incoming) = running.server.connect_in_memory(SessionKind::Full);
        let refused = Client::over_memory(outgoing, incoming)
            .hello(&hello())
            .await;
        assert!(
            matches!(refused, Err(ritornel_client::Error::Transport(_))),
            "{refused:?}"
        );
        first.servers().await.unwrap();
        second.servers().await.unwrap();
        // The routes are not sessions, and are answered all the same.
        ritornel_client::health(running.endpoint(), &running.credential())
            .await
            .unwrap();

        // When one closes, its place is free. The server learns of the
        // close a moment after the client, so this asks until it has.
        drop(first);
        let third = loop {
            match upgrade(running.endpoint(), &[("authorization", &authorization)]).await {
                Ok(third) => break third,
                Err(503) => tokio::task::yield_now().await,
                Err(other) => panic!("the upgrade was refused with {other}"),
            }
        };
        drop(third);
    })
    .await;
}

#[tokio::test]
async fn a_taken_default_port_falls_back_to_one_the_system_picks() {
    bounded(async {
        // Hold the default port, unless something on this machine already
        // does. Either way it is taken when the server starts.
        let held = std::net::TcpListener::bind("127.0.0.1:4747");
        if let Err(taken) = &held {
            assert_eq!(taken.kind(), std::io::ErrorKind::AddrInUse, "{taken}");
        }

        let scratch = Scratch::new();
        let server = start_on(&scratch.data_dir(), Listen::DEFAULT)
            .await
            .unwrap();
        let endpoint = server.endpoint();
        assert_eq!(endpoint.ip().to_string(), "127.0.0.1");
        assert!(
            endpoint.port() != 4747 && endpoint.port() != 0,
            "{endpoint}"
        );

        // `server.json` names the port really bound, and a client that
        // reads it reaches this server.
        let named: Value = serde_json::from_str(
            &std::fs::read_to_string(scratch.data_dir().join("server.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            named,
            json!({
                "pid": std::process::id(),
                "endpoint": format!("127.0.0.1:{}", endpoint.port()),
                "serverId": server.server_id(),
                "contractVersion": CONTRACT_VERSION,
            })
        );
        let found = local::find(&scratch.data_dir()).await.unwrap();
        assert_eq!(found.endpoint, endpoint);
        assert_eq!(found.descriptor.server_id, server.server_id());

        server.stop().await.unwrap();
        drop(held);
    })
    .await;
}

#[tokio::test]
async fn a_chosen_port_that_is_taken_stops_the_start() {
    bounded(async {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let scratch = Scratch::new();
        let refused = start_on(&scratch.data_dir(), Listen::on_port(port)).await;
        let Err(error @ Error::Listen { .. }) = &refused else {
            panic!("{refused:?}");
        };
        assert!(
            error
                .to_string()
                .starts_with(&format!("cannot listen on 127.0.0.1:{port}: ")),
            "{error}"
        );
        // It told nobody it was there, and let the directory go.
        assert!(!scratch.data_dir().join("server.json").exists());
        start_on(&scratch.data_dir(), Listen::on_port(0))
            .await
            .unwrap()
            .stop()
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_local_client_finds_the_server_through_its_data_directory() {
    bounded(async {
        let running = Running::start().await;
        let data_dir = running.data_dir();

        let found = local::find(&data_dir).await.unwrap();
        assert_eq!(found.endpoint, running.endpoint());
        assert_eq!(found.descriptor, running.public_descriptor());
        let client = found.connect().await.unwrap();
        assert_eq!(
            client.hello(&hello()).await.unwrap().session_kind,
            SessionKind::Full
        );
        assert_eq!(
            client.status().await.unwrap().server_id,
            running.server.server_id()
        );
        // The credential it read is the local one, and it is not printed.
        ritornel_client::health(found.endpoint, found.credential())
            .await
            .unwrap();
        assert!(!format!("{found:?}").contains(&running.token()));

        // A second server on the directory is told who has it and where.
        let second = HeldDir::take(data_dir.clone()).unwrap_err();
        assert_eq!(
            second.to_string(),
            format!(
                "data directory in use by pid {} at {}: {}",
                std::process::id(),
                running.endpoint(),
                data_dir.display()
            )
        );

        running.server.stop().await.unwrap();
        let gone = local::find(&data_dir).await;
        assert!(
            matches!(
                gone,
                Err(ritornel_client::Error::NotRunning {
                    why: NotRunning::NoServerJson,
                    ..
                })
            ),
            "{gone:?}"
        );
    })
    .await;
}

/// Holds a data directory's lock as a server does, with this test's pid in
/// the lock file, for a `server.json` a test writes by hand to be believed.
fn hold(data_dir: &Path) -> std::fs::File {
    let path = data_dir.join("server.lock");
    std::fs::write(&path, format!("{}\n", std::process::id())).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    // Windows enforces a lock on what it covers, so a server there holds
    // one byte past the end and leaves the pid readable.
    #[cfg(unix)]
    file.try_lock().unwrap();
    #[cfg(windows)]
    assert!(
        ritornel_windows::lock::try_lock(
            &file,
            ritornel_contract::local::WINDOWS_LOCK_BYTE,
            1,
            ritornel_windows::lock::Hold::Exclusive,
        )
        .unwrap()
    );
    file
}

fn not_running(found: Result<local::LocalServer, ritornel_client::Error>) -> NotRunning {
    match found {
        Err(ritornel_client::Error::NotRunning { why, .. }) => why,
        other => panic!("{other:?}"),
    }
}

/// The review's attack. A server is killed, and `server.json` stays. Its
/// ID is public and its port is free, so anything on the machine can take
/// the port and answer as that server.
#[tokio::test]
async fn an_impersonator_on_a_killed_servers_port_is_sent_no_credential() {
    bounded(async {
        let running = Running::start().await;
        let (data_dir, endpoint, token) = (running.data_dir(), running.endpoint(), running.token());
        let descriptor = serde_json::to_string(&running.public_descriptor()).unwrap();
        let left = std::fs::read_to_string(data_dir.join("server.json")).unwrap();

        // What a kill leaves: the lock file with the pid in it and nobody
        // holding it, the credential, and `server.json`. A stop removes
        // that last one, so it is put back.
        let Running { server, scratch } = running;
        server.stop().await.unwrap();
        std::fs::write(data_dir.join("server.json"), &left).unwrap();

        // The impersonator takes the port, and answers with the same
        // descriptor, server ID and all.
        let listener = tokio::net::TcpListener::bind(endpoint).await.unwrap();
        let impersonator = Stranger::on(listener, &descriptor);
        assert_eq!(
            serde_json::to_string(&ritornel_client::descriptor(endpoint).await.unwrap()).unwrap(),
            descriptor
        );

        // A client does what `ritornelctl status` does.
        let found = local::find(&data_dir).await;
        if let Ok(found) = &found {
            let _ = found.connect().await;
        }

        // It was asked nothing, let alone sent the credential: the one
        // request it saw is this test's own, above.
        let seen = impersonator.seen();
        assert!(
            !format!("{seen:?}").contains(&token),
            "the impersonator was sent the credential"
        );
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(not_running(found), NotRunning::Killed);
        drop(scratch);
    })
    .await;
}

/// A server that was found, and is gone by the time the client connects.
#[tokio::test]
async fn a_server_that_goes_between_being_found_and_connected_to_is_sent_no_credential() {
    bounded(async {
        let running = Running::start().await;
        let (data_dir, endpoint, token) = (running.data_dir(), running.endpoint(), running.token());
        let descriptor = serde_json::to_string(&running.public_descriptor()).unwrap();
        let found = local::find(&data_dir).await.unwrap();

        let left = std::fs::read_to_string(data_dir.join("server.json")).unwrap();
        let Running { server, scratch } = running;
        server.stop().await.unwrap();
        std::fs::write(data_dir.join("server.json"), &left).unwrap();
        let listener = tokio::net::TcpListener::bind(endpoint).await.unwrap();
        let impersonator = Stranger::on(listener, &descriptor);

        // The connection is made, to the impersonator, and nothing is sent
        // on it.
        let refused = found.connect().await;
        assert!(
            matches!(
                refused,
                Err(ritornel_client::Error::NotRunning {
                    why: NotRunning::Killed,
                    ..
                })
            ),
            "{refused:?}"
        );
        let seen = impersonator.seen();
        assert!(seen.is_empty(), "{seen:?}");
        assert!(!format!("{seen:?}").contains(&token));
        drop(scratch);
    })
    .await;
}

/// A server is alive, and what answers where its `server.json` points is
/// not it. That takes a fault, or someone who can already write the data
/// directory. The descriptor still decides, and still no credential goes.
#[tokio::test]
async fn what_answers_for_a_live_server_and_is_not_it_is_sent_no_credential() {
    bounded(async {
        let running = Running::start().await;
        let dir = Scratch::new();
        std::fs::create_dir_all(dir.data_dir()).unwrap();
        let token = "0".repeat(64);
        std::fs::write(dir.data_dir().join("credential"), &token).unwrap();
        let _held = hold(&dir.data_dir());
        let name = |endpoint: SocketAddr, server_id: Id| {
            let contents = json!({
                "pid": std::process::id(),
                "endpoint": endpoint,
                "serverId": server_id,
                "contractVersion": CONTRACT_VERSION,
            });
            std::fs::write(dir.data_dir().join("server.json"), contents.to_string()).unwrap();
        };

        // Another server: this test's.
        name(running.endpoint(), Id::from_bytes([0x11; 16]));
        assert_eq!(
            not_running(local::find(&dir.data_dir()).await),
            NotRunning::AnotherServer(running.endpoint())
        );

        // Something that is not a server at all.
        let stranger = Stranger::start(r#"{"hello":"world"}"#).await;
        name(stranger.endpoint, Id::from_bytes([0x11; 16]));
        assert_eq!(
            not_running(local::find(&dir.data_dir()).await),
            NotRunning::AnotherServer(stranger.endpoint)
        );
        assert!(!format!("{:?}", stranger.seen()).contains(&token));

        // Nothing. Another program may take the port at any moment, so
        // either answer is right.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let nobody = free.local_addr().unwrap();
        drop(free);
        name(nobody, Id::from_bytes([0x11; 16]));
        let why = not_running(local::find(&dir.data_dir()).await);
        assert!(
            why == NotRunning::NoAnswer(nobody) || why == NotRunning::AnotherServer(nobody),
            "{why:?}"
        );
    })
    .await;
}

/// A client may look at the lock at the very instant a server starts. The
/// server must not take the look for another server.
#[test]
fn clients_looking_for_a_server_do_not_keep_one_from_starting() {
    let scratch = Scratch::new();
    let data_dir = scratch.data_dir();
    let looking = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let lookers: Vec<_> = (0..3)
        .map(|_| {
            let (data_dir, looking) = (data_dir.clone(), Arc::clone(&looking));
            std::thread::spawn(move || {
                let mut looks = 0_u64;
                while looking.load(Ordering::SeqCst) {
                    local::probe(&data_dir).unwrap();
                    looks += 1;
                }
                looks
            })
        })
        .collect();

    // A server takes the directory and lets it go, over and over.
    let started = Instant::now();
    for start in 0..300 {
        let held = HeldDir::take(data_dir.clone());
        assert!(held.is_ok(), "start {start}: {held:?}");
        // While it has it, every look says so.
        assert_eq!(
            local::probe(&data_dir).unwrap(),
            local::Lock::Held {
                pid: Some(std::process::id())
            }
        );
    }
    looking.store(false, Ordering::SeqCst);
    let looks: u64 = lookers
        .into_iter()
        .map(|looker| looker.join().unwrap())
        .sum();
    // The lookers were really in the way, and the starts were not slow.
    assert!(looks > 300, "{looks} looks");
    assert!(started.elapsed() < WAIT, "{:?}", started.elapsed());
}

#[tokio::test]
async fn a_server_that_speaks_another_contract_version_is_not_connected_to() {
    bounded(async {
        let server_id = Id::from_bytes([0x22; 16]);
        let newer = json!({
            "serverId": server_id,
            "name": "from the future",
            "role": "hub",
            "contractVersion": CONTRACT_VERSION + 1,
            "capabilities": [],
            "limits": { "maxMessageBytes": 1024 },
        });
        let stranger = Stranger::start(&newer.to_string()).await;
        let scratch = Scratch::new();
        std::fs::create_dir_all(scratch.data_dir()).unwrap();
        let _held = hold(&scratch.data_dir());
        let named = json!({
            "pid": std::process::id(),
            "endpoint": stranger.endpoint,
            "serverId": server_id,
            "contractVersion": CONTRACT_VERSION + 1,
        });
        std::fs::write(scratch.data_dir().join("server.json"), named.to_string()).unwrap();

        let found = local::find(&scratch.data_dir()).await;
        let Err(error @ ritornel_client::Error::ContractVersion { .. }) = &found else {
            panic!("{found:?}");
        };
        assert_eq!(
            error.to_string(),
            format!(
                "the server speaks contract version {} and this program version \
                 {CONTRACT_VERSION}: use the two from one release",
                CONTRACT_VERSION + 1
            )
        );
    })
    .await;
}

/// Something on loopback that answers every request with the same body,
/// and keeps what it was asked. It stands for whatever program a dead
/// server's port went to.
struct Stranger {
    endpoint: SocketAddr,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
    serving: tokio::task::JoinHandle<()>,
}

impl Stranger {
    async fn start(body: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self::on(listener, body)
    }

    /// Answers on a listener that is already bound.
    fn on(listener: tokio::net::TcpListener, body: &str) -> Self {
        let endpoint = listener.local_addr().unwrap();
        let body = body.to_owned();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let router = axum::Router::new().fallback({
            let seen = Arc::clone(&seen);
            move |request: axum::extract::Request| {
                // The whole head: the path, and every header with its value.
                seen.lock()
                    .unwrap()
                    .push(format!("{} {:?}", request.uri(), request.headers()));
                let body = body.clone();
                async move { body }
            }
        });
        let serving = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            endpoint,
            seen,
            serving,
        }
    }

    /// The head of every request it has had.
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Stranger {
    fn drop(&mut self) {
        self.serving.abort();
    }
}
