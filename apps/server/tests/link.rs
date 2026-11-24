//! The link between a hub and a satellite: two real servers inside the
//! test process, each on a data directory of its own and a port the system
//! picks, joined over loopback as two machines are over their network.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ritornel_client::contract::methods::{
    LibraryStats, LinkInvite, LinkJoin, LinkListen, LinkPing, LinkStatusCall, PlaybackGet,
    PlaybackPause, PlaybackPlay, QueueWindow, RatingsSet, ServerSetSettings, ServerSettingsCall,
    SourcesAdd,
};
use ritornel_client::contract::{
    Awake, CONTRACT_VERSION, ClientKind, ErrorCode, Event, HelloParams, InviteLink,
    LinkInviteParams, LinkJoinParams, LinkListenParams, LinkPingParams, LinkRequest, LinkStatus,
    ListKind, ListOpenParams, ListenTaken, NoParams, PlaySource, PlaybackCommandParams,
    PlaybackPlayParams, PlaybackStatus, Query, QueueWindowParams, RatingSetParams, Row, ServerRole,
    SetBy, SetServerSettingsParams, SourcesAddParams, Topic, Window, capability, hex,
    link_request_proof, route,
};
use ritornel_client::{Client, Credential};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Output, PlaybackRun, Run, Server};

/// How long a test may take before it is failed.
const WAIT: Duration = Duration::from_secs(60);

/// How long a slow hub waits before each part of a master it sends: far
/// longer than anything a test does while the pull is under way, so that
/// none can outlast it however slow the machine.
const PULL_PACE: Duration = Duration::from_secs(600);

async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(WAIT, test)
        .await
        .expect("the test did not finish in time");
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = tempfile::Builder::new()
            .prefix("ritornel-link-")
            .tempdir()
            .unwrap()
            .keep();
        Self(path)
    }

    fn data_dir(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn start(scratch: &Scratch, name: &str) -> Server {
    start_on(scratch, name, Listen::on_port(0)).await
}

async fn start_on(scratch: &Scratch, name: &str, listen: Listen) -> Server {
    Server::start(
        HeldDir::take(scratch.data_dir(name)).unwrap(),
        listen,
        Arc::new(SystemClock),
        Arc::new(OsEntropy),
    )
    .await
    .unwrap()
}

/// A server with a player on the null output, `speed` times real time,
/// and a cache of `cache_bytes` for the masters it pulls, or the default.
async fn start_playing_with(
    scratch: &Scratch,
    name: &str,
    speed: u32,
    cache_bytes: Option<u64>,
) -> Server {
    Server::start_run(
        HeldDir::take(scratch.data_dir(name)).unwrap(),
        Listen::on_port(0),
        Arc::new(SystemClock),
        Arc::new(OsEntropy),
        Bounds::default(),
        Run {
            playback: PlaybackRun {
                output: Some(Output::Null { speed }),
                ..PlaybackRun::default()
            },
            cache_bytes,
            ..Run::default()
        },
    )
    .await
    .unwrap()
}

fn key() -> ritornel_client::contract::Id {
    ritornel_client::idempotency_key().unwrap()
}

/// An album of three tracks from the audio fixtures, in the scratch
/// directory.
fn album(scratch: &Scratch) -> PathBuf {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
    let folder = scratch.0.join("music");
    for (to, fixture) in [
        ("Tin Weather/01.flac", "a.flac"),
        ("Tin Weather/02.flac", "b.flac"),
        ("Tin Weather/03.mp3", "a.mp3"),
    ] {
        let to = folder.join(to);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixtures.join(fixture), to).unwrap();
    }
    folder.canonicalize().unwrap()
}

/// Adds the album to the hub's library and scans it, and waits for the
/// scan to end.
async fn scan(scratch: &Scratch, hub: &mut Client) {
    let folder = album(scratch);
    let source = hub
        .add_source(key(), "music", folder.to_str().unwrap())
        .await
        .unwrap()
        .source;
    hub.subscribe(&[Topic::Jobs]).await.unwrap();
    let job = hub.start_scan(key(), source.source_id).await.unwrap().job;
    loop {
        if let Event::Jobs(event) = hub.next_event().await.unwrap()
            && event.job.job_id == job.job_id
            && event.job.state.ended()
        {
            break;
        }
    }
    hub.unsubscribe(&[Topic::Jobs]).await.unwrap();
}

/// A hub with the album scanned, and a satellite linked to it, with a
/// client of each.
async fn linked(scratch: &Scratch) -> (Server, Client, Server, Client) {
    let (hub, mut hub_client, desk, desk_client) = two(scratch).await;
    scan(scratch, &mut hub_client).await;
    join(&desk_client, &invite(&hub, &hub_client).await)
        .await
        .unwrap();
    (hub, hub_client, desk, desk_client)
}

fn songs() -> ListOpenParams {
    ListOpenParams {
        kind: ListKind::Songs,
        query: Query::default(),
        window: Some(Window {
            offset: 0,
            limit: 100,
        }),
        replaces: None,
    }
}

/// A client of `server` with the local credential, that has said hello.
async fn client(scratch: &Scratch, name: &str, server: &Server) -> Client {
    let token = std::fs::read_to_string(scratch.data_dir(name).join("credential")).unwrap();
    let client = Client::connect(server.endpoint(), &Credential::new(token))
        .await
        .unwrap();
    client
        .hello(&HelloParams {
            name: "link-test".to_owned(),
            kind: ClientKind::Cli,
            version: "1".to_owned(),
            contract_version: CONTRACT_VERSION,
        })
        .await
        .unwrap();
    client
}

fn code(error: &ritornel_client::Error) -> Option<ErrorCode> {
    match error {
        ritornel_client::Error::Call(error) => Some(error.kind()),
        _ => None,
    }
}

/// What a server holding the key made from `seed` sends to join with
/// `invite`, as the server `server_id`.
fn request(invite: &InviteLink, server_id: ritornel_client::contract::Id, seed: u8) -> LinkRequest {
    let signer = ritornel_engine::Signer::from_seed([seed; 32]);
    LinkRequest {
        secret: hex::encode(&invite.secret),
        server_id,
        name: "invented".to_owned(),
        public_key: hex::encode(&signer.public_key()),
        proof: hex::encode(&signer.sign(&link_request_proof(invite.hub, &invite.secret))),
        contract_version: CONTRACT_VERSION,
    }
}

async fn post(hub: &Server, asked: &LinkRequest) -> (u16, String) {
    let answer = ritornel_client::post(hub.endpoint(), route::LINK, None, asked)
        .await
        .unwrap();
    (
        answer.status,
        String::from_utf8_lossy(&answer.body).into_owned(),
    )
}

/// An invite from `hub`, offering its loopback address.
async fn invite(hub: &Server, client: &Client) -> String {
    client
        .call::<LinkInvite>(&LinkInviteParams {
            routes: vec![hub.endpoint().to_string()],
        })
        .await
        .unwrap()
        .link
}

async fn join(client: &Client, link: &str) -> Result<LinkStatus, ritornel_client::Error> {
    client
        .call::<LinkJoin>(&LinkJoinParams {
            idempotency_key: ritornel_client::idempotency_key().unwrap(),
            link: link.to_owned(),
        })
        .await
}

/// A hub with a library, and a server with none.
async fn two(scratch: &Scratch) -> (Server, Client, Server, Client) {
    let hub = start(scratch, "hub").await;
    let hub_client = client(scratch, "hub", &hub).await;
    hub_client
        .create_library(ritornel_client::idempotency_key().unwrap())
        .await
        .unwrap();
    let desk = start(scratch, "desk").await;
    let desk_client = client(scratch, "desk", &desk).await;
    (hub, hub_client, desk, desk_client)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_joins_a_hub_with_an_invite_and_becomes_its_satellite() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, desk, desk_client) = two(&scratch).await;
        let descriptor = ritornel_client::descriptor(hub.endpoint()).await.unwrap();
        assert!(descriptor.has(capability::LINK));

        let link = invite(&hub, &hub_client).await;
        let parsed = InviteLink::parse(&link).unwrap();
        assert_eq!(parsed.hub, hub.server_id());
        assert_eq!(hex::encode(&parsed.key), hub.public_key());

        let joined = join(&desk_client, &link).await.unwrap();
        assert_eq!(joined.role, ServerRole::Satellite);
        let held = joined.hub.clone().unwrap();
        assert_eq!(held.server_id, hub.server_id());
        assert_eq!(held.routes, vec![hub.endpoint().to_string()]);
        let library = hub_client.status().await.unwrap().library.unwrap();
        assert_eq!(held.library_id, library.library_id);
        assert_eq!(
            desk_client.status().await.unwrap().role,
            ServerRole::Satellite
        );

        // The hub knows it, and has it in the library's registry.
        let at_hub = hub_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(at_hub.role, ServerRole::Hub);
        assert_eq!(
            at_hub
                .satellites
                .iter()
                .map(|satellite| satellite.server_id)
                .collect::<Vec<_>>(),
            vec![desk.server_id()]
        );

        // Joining again with the same link answers with the link it made.
        assert_eq!(join(&desk_client, &link).await.unwrap(), joined);

        // The satellite stays one when it starts again.
        drop(desk_client);
        desk.stop().await.unwrap();
        let desk = start(&scratch, "desk").await;
        let desk_client = client(&scratch, "desk", &desk).await;
        let status = desk_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(status.role, ServerRole::Satellite);
        assert_eq!(status.hub.unwrap().server_id, hub.server_id());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_may_be_a_name_resolved_at_each_dial_and_a_name_for_another_server_is_refused() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, _desk, desk_client) = two(&scratch).await;
        let link = invite(&hub, &hub_client).await;
        let by_name = |port: u16| {
            let mut named = InviteLink::parse(&link).unwrap();
            named.routes = vec![format!("localhost:{port}")];
            named.to_string()
        };

        // A name that answers with another server is harmless: that server
        // cannot prove it holds the hub's key, and is sent nothing else.
        let other = start(&scratch, "other").await;
        let refused = join(&desk_client, &by_name(other.endpoint().port()))
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("not the hub the link names"),
            "{refused}"
        );

        // A name that answers with the hub links, after one that resolves
        // to nothing. The routes are kept as the invite named them, the
        // name as a name, to be resolved and ordered again each time the
        // satellite dials.
        // A tailnet's route named first is tried last, after this network.
        let mut named = InviteLink::parse(&by_name(hub.endpoint().port())).unwrap();
        named.routes.insert(0, "nowhere.invalid:4747".to_owned());
        named.routes.insert(0, "100.64.0.1:9".to_owned());
        // That the silent tailnet address holds up nothing, and the order
        // the routes are tried in, are `first_answer`'s and `candidates`'s,
        // each tested exactly on its own; here the join through them is.
        let status = join(&desk_client, &named.to_string()).await.unwrap();
        let hub_status = status.hub.unwrap();
        assert_eq!(hub_status.server_id, hub.server_id());
        assert_eq!(hub_status.routes, named.routes);
        let stats = desk_client
            .call::<LibraryStats>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(stats.artists, 0);
        // It says how it reached the hub: the route as the link names it,
        // and the address that was.
        let reached = desk_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap()
            .hub
            .unwrap();
        let port = hub.endpoint().port();
        assert_eq!(reached.answers, Some(true));
        assert_eq!(
            reached.route.as_deref(),
            Some(format!("localhost:{port} (127.0.0.1:{port})").as_str())
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invite_works_once_and_only_for_the_hub_it_names() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, _desk, desk_client) = two(&scratch).await;
        let link = invite(&hub, &hub_client).await;

        // Another machine answering at the route is not the hub: the
        // satellite sends it nothing but a nonce.
        let other = start(&scratch, "other").await;
        let mut forged = InviteLink::parse(&link).unwrap();
        forged.routes = vec![other.endpoint().to_string()];
        let refused = join(&desk_client, &forged.to_string()).await.unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Unavailable), "{refused}");
        assert!(
            refused.to_string().contains("not the hub the link names"),
            "{refused}"
        );

        // A route off the private network is never dialled.
        let mut public = InviteLink::parse(&link).unwrap();
        public.routes = vec!["8.8.8.8:4747".to_owned()];
        let refused = join(&desk_client, &public.to_string()).await.unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Invalid), "{refused}");

        // A satellite that does not prove it holds the key it names is
        // refused, and the invite is not spent.
        let parsed = InviteLink::parse(&link).unwrap();
        let mut unproved = request(&parsed, other.server_id(), 1);
        unproved.proof = request(&parsed, other.server_id(), 2).proof;
        assert_eq!(post(&hub, &unproved).await.0, 400);

        // The secret is spent by the satellite that uses it first.
        assert_eq!(
            post(&hub, &request(&parsed, other.server_id(), 1)).await.0,
            200
        );
        assert_eq!(
            post(&hub, &request(&parsed, other.server_id(), 1)).await.0,
            403
        );
        let refused = join(&desk_client, &link).await.unwrap_err();
        assert!(
            refused.to_string().contains("refused the link"),
            "{refused}"
        );
        // The hub answered, so it is not said to be unreachable.
        assert_eq!(code(&refused), Some(ErrorCode::Conflict), "{refused}");
        // And a page in a browser may not try at all. It is refused on its
        // origin alone, before a body is read, so it sends none: a body the
        // hub never reads could reset the connection before the answer.
        let page = tokio::task::spawn_blocking({
            let endpoint = hub.endpoint();
            move || {
                use std::io::{Read, Write};
                let mut stream = std::net::TcpStream::connect(endpoint).unwrap();
                write!(
                    stream,
                    "POST /v1/link HTTP/1.1\r\nHost: {endpoint}\r\nOrigin: http://example.invalid\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                let mut answer = String::new();
                stream.read_to_string(&mut answer).unwrap();
                answer
            }
        })
        .await
        .unwrap();
        assert!(page.starts_with("HTTP/1.1 403"), "{page}");
        assert_eq!(
            desk_client.status().await.unwrap().role,
            ServerRole::Unassigned
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_of_another_contract_version_is_refused() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, desk, _desk_client) = two(&scratch).await;
        let parsed = InviteLink::parse(&invite(&hub, &hub_client).await).unwrap();
        let asked = LinkRequest {
            contract_version: CONTRACT_VERSION + 1,
            ..request(&parsed, desk.server_id(), 1)
        };
        let (status, body) = post(&hub, &asked).await;
        assert_eq!(status, 409);
        assert!(body.contains("update the hub"), "{body}");
        // The invite is not spent by a refusal.
        assert_eq!(
            post(&hub, &request(&parsed, desk.server_id(), 1)).await.0,
            200
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_hub_listening_beyond_loopback_invites_and_only_an_empty_server_joins() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, _desk, desk_client) = two(&scratch).await;
        // On loopback, with no route given, there is nowhere to offer.
        let refused = hub_client
            .call::<LinkInvite>(&LinkInviteParams::default())
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Invalid));
        assert!(
            refused.to_string().contains("--listen private"),
            "{refused}"
        );
        // A server with no library invites no one.
        let refused = desk_client
            .call::<LinkInvite>(&LinkInviteParams {
                routes: vec!["127.0.0.1:1".to_owned()],
            })
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Invalid));
        // A hub joins no other hub.
        let other = start(&scratch, "other").await;
        let other_client = client(&scratch, "other", &other).await;
        other_client
            .create_library(ritornel_client::idempotency_key().unwrap())
            .await
            .unwrap();
        let refused = join(&other_client, &invite(&hub, &hub_client).await)
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Conflict), "{refused}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_opens_a_socket_at_its_hub_with_its_link_session_and_no_ticket() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, desk, desk_client) = two(&scratch).await;
        join(&desk_client, &invite(&hub, &hub_client).await)
            .await
            .unwrap();
        drop(desk_client);
        desk.stop().await.unwrap();
        // The token is the satellite's, in its server.db, as it will
        // present it.
        let token = {
            let db =
                rusqlite::Connection::open(scratch.data_dir("desk").join("server.db")).unwrap();
            db.query_row("SELECT token FROM hub_link", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
        };
        let credential = Credential::new(&token);
        let linked = Client::connect(hub.endpoint(), &credential).await.unwrap();
        linked
            .hello(&HelloParams {
                name: "desk".to_owned(),
                kind: ClientKind::Cli,
                version: "1".to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await
            .unwrap();
        assert_eq!(linked.status().await.unwrap().role, ServerRole::Hub);
        // Through its link it may not invite another server.
        let refused = linked
            .call::<LinkInvite>(&LinkInviteParams {
                routes: vec![hub.endpoint().to_string()],
            })
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Forbidden));
        // Nor touch what is the hub's own machine's: its folders and its
        // backups.
        for method in ["sources.add", "server.backup", "history.export"] {
            let refused = linked
                .call_raw(method, serde_json::json!({}))
                .await
                .unwrap_err();
            assert_eq!(code(&refused), Some(ErrorCode::Forbidden), "{method}");
        }
        // And it is given no ticket.
        assert!(matches!(
            ritornel_client::ticket(hub.endpoint(), &credential).await,
            Err(ritornel_client::Error::Refused { status: 403 })
        ));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invite_does_not_take_over_a_satellite_that_is_linked_already() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, desk, desk_client) = two(&scratch).await;
        join(&desk_client, &invite(&hub, &hub_client).await)
            .await
            .unwrap();
        // Someone else with an invite names the satellite's ID, with a key
        // of their own.
        let parsed = InviteLink::parse(&invite(&hub, &hub_client).await).unwrap();
        let (status, body) = post(&hub, &request(&parsed, desk.server_id(), 9)).await;
        assert_eq!(status, 409, "{body}");
        let at_hub = hub_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(at_hub.satellites.len(), 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_reads_the_hubs_library_through_its_link() {
    bounded(async {
        let scratch = Scratch::new();
        let (_hub, hub_client, _desk, desk_client) = linked(&scratch).await;
        let at_hub = hub_client.call::<LibraryStats>(&NoParams {}).await.unwrap();
        let through = desk_client
            .call::<LibraryStats>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(through, at_hub);

        let at_hub = hub_client.list_open(&songs()).await.unwrap();
        let through = desk_client.list_open(&songs()).await.unwrap();
        assert!(through.total > 0);
        assert_eq!(through.total, at_hub.total);
        assert_eq!(through.rows, at_hub.rows);
        // The view is held at the hub for the satellite's client, and read
        // through it.
        let window = desk_client.list_window(&through.view, 0, 10).await.unwrap();
        assert_eq!(window.rows, at_hub.rows);

        // The satellite says whose library it reads, and that it answers.
        let status = desk_client.status().await.unwrap();
        let library = hub_client.status().await.unwrap().library.unwrap();
        assert_eq!(status.library.unwrap().library_id, library.library_id);
        let hub = status.hub.unwrap();
        assert_eq!(hub.answers, Some(true));
        assert!(hub.since_ms.is_some() && hub.problem.is_none());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hubs_library_events_reach_a_satellites_client() {
    bounded(async {
        let scratch = Scratch::new();
        let (_hub, hub_client, _desk, mut desk_client) = linked(&scratch).await;
        desk_client.subscribe(&[Topic::Library]).await.unwrap();
        let Row::Song(song) = hub_client.list_open(&songs()).await.unwrap().rows[0].clone() else {
            panic!("not a song");
        };
        // A change made at the hub, by any client.
        hub_client
            .call::<RatingsSet>(&RatingSetParams {
                idempotency_key: key(),
                recording_id: song.recording_id,
                rating: Some(80),
            })
            .await
            .unwrap();
        loop {
            if let Event::Library(_) = desk_client.next_event().await.unwrap() {
                break;
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_names_the_hubs_folders_is_done_on_the_hub() {
    bounded(async {
        let scratch = Scratch::new();
        let (_hub, _hub_client, _desk, desk_client) = linked(&scratch).await;
        let refused = desk_client
            .call::<SourcesAdd>(&SourcesAddParams {
                idempotency_key: key(),
                name: "music".to_owned(),
                path: "/music".to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Unsupported), "{refused}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn while_the_hub_is_away_a_satellite_says_so_and_comes_back_with_it() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, _desk, desk_client) = linked(&scratch).await;
        desk_client
            .call::<LibraryStats>(&NoParams {})
            .await
            .unwrap();
        let port = hub.endpoint().port();
        drop(hub_client);
        hub.stop().await.unwrap();

        let refused = desk_client
            .call::<LibraryStats>(&NoParams {})
            .await
            .unwrap_err();
        let ritornel_client::Error::Call(error) = &refused else {
            panic!("{refused}");
        };
        assert_eq!(error.kind(), ErrorCode::Unavailable);
        assert_eq!(error.data.details["reason"], "hub_unreachable");
        assert!(error.data.details.contains_key("sinceMs"));
        let hub_status = desk_client.status().await.unwrap().hub.unwrap();
        assert_eq!(hub_status.answers, Some(false));
        assert!(hub_status.problem.is_some());
        // Its library is still the hub's: nothing says it is gone.
        assert!(desk_client.status().await.unwrap().library.is_some());

        // The hub comes back where it was, and the satellite reads it
        // again, on the same session.
        // The port was let go a moment ago, and another test's server, or
        // the system, may hold it a little longer: it is tried until free.
        let hub = loop {
            let tried = Server::start(
                HeldDir::take(scratch.data_dir("hub")).unwrap(),
                Listen::on_port(port),
                Arc::new(SystemClock),
                Arc::new(OsEntropy),
            )
            .await;
            match tried {
                Ok(hub) => break hub,
                Err(ritornel_server::Error::Listen { source, .. })
                    if source.kind() == std::io::ErrorKind::AddrInUse =>
                {
                    // sleep: between tries; the test's WAIT bounds them.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(error) => panic!("the hub did not start again: {error}"),
            }
        };
        loop {
            match desk_client.call::<LibraryStats>(&NoParams {}).await {
                Ok(_) => break,
                // sleep: between looks; the test's WAIT bounds them.
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
        let hub_status = desk_client.status().await.unwrap().hub.unwrap();
        assert_eq!(hub_status.answers, Some(true));
        drop(hub);
    })
    .await;
}

/// A hub with the album scanned, and a satellite with a player linked to
/// it, with a client of each.
async fn linked_player(scratch: &Scratch) -> (Server, Client, Server, Client) {
    linked_player_at(scratch, 100).await
}

/// As [`linked_player`], with the satellite playing at `speed` times real
/// time.
async fn linked_player_at(scratch: &Scratch, speed: u32) -> (Server, Client, Server, Client) {
    linked_player_with(scratch, speed, None).await
}

/// As [`linked_player_at`], with a cache of `cache_bytes` on the satellite.
async fn linked_player_with(
    scratch: &Scratch,
    speed: u32,
    cache_bytes: Option<u64>,
) -> (Server, Client, Server, Client) {
    let hub = start(scratch, "hub").await;
    let mut hub_client = client(scratch, "hub", &hub).await;
    hub_client.create_library(key()).await.unwrap();
    scan(scratch, &mut hub_client).await;
    let desk = start_playing_with(scratch, "desk", speed, cache_bytes).await;
    let desk_client = client(scratch, "desk", &desk).await;
    join(&desk_client, &invite(&hub, &hub_client).await)
        .await
        .unwrap();
    (hub, hub_client, desk, desk_client)
}

/// Plays `source` on the satellite and waits until the player has stopped
/// at the end of it. Whether it said it was buffering, waiting for a master
/// to be pulled, on the way.
async fn play_to_the_end(client: &mut Client, source: PlaySource) -> bool {
    client.subscribe(&[Topic::Playback]).await.unwrap();
    client
        .call::<PlaybackPlay>(&PlaybackPlayParams {
            idempotency_key: key(),
            source,
            start: None,
            context: None,
            shuffle: None,
        })
        .await
        .unwrap();
    let (mut played, mut buffered) = (false, false);
    loop {
        if let Event::Playback(event) = client.next_event().await.unwrap() {
            buffered |= event.state.status == PlaybackStatus::Buffering;
            played |= event.state.status == PlaybackStatus::Playing;
            if played && event.state.status == PlaybackStatus::Stopped {
                break;
            }
        }
    }
    client.unsubscribe(&[Topic::Playback]).await.unwrap();
    buffered
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_keeps_no_more_of_the_hubs_music_than_its_cache_size() {
    bounded(async {
        let scratch = Scratch::new();
        // Two more songs beside the album, each with a master of its own.
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
        for (folder, fixture) in [
            ("Somebody Else", "a.retagged.flac"),
            ("Field Notes", "b.no-artist.flac"),
        ] {
            let folder = scratch.0.join("music").join(folder);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::copy(fixtures.join(fixture), folder.join("01.flac")).unwrap();
        }
        // A cache smaller than any master: each is let go once two newer
        // are kept, the one playing and the one pulled after it.
        let (_hub, _hub_client, _desk, mut desk_client) =
            linked_player_with(&scratch, 100, Some(1)).await;
        let songs = desk_client.list_open(&songs()).await.unwrap();
        let ids: Vec<_> = songs
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Song(song) => Some(song.appearance_id),
                _ => None,
            })
            .collect();
        assert!(ids.len() >= 3, "{ids:?}");
        play_to_the_end(&mut desk_client, PlaySource::Tracks { ids }).await;
        // Each played, and only the last two are kept.
        let window = desk_client
            .call::<QueueWindow>(&QueueWindowParams {
                offset: 0,
                limit: None,
                order: None,
            })
            .await
            .unwrap();
        assert!(
            window
                .entries
                .iter()
                .all(|entry| entry.failed_reason.is_none()),
            "{window:?}"
        );
        let kept = std::fs::read_dir(scratch.data_dir("desk").join("cache").join("audio"))
            .unwrap()
            .count();
        assert_eq!(kept, 2);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_answers_and_stops_while_a_slow_pull_is_under_way() {
    bounded(async {
        let scratch = Scratch::new();
        // A hub that sends each master slowly, so that the satellite's pull
        // is held open for longer than any of what follows may take.
        let hub = Server::start_run(
            HeldDir::take(scratch.data_dir("hub")).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            Bounds::default(),
            Run {
                audio_pace: Some(PULL_PACE),
                ..Run::default()
            },
        )
        .await
        .unwrap();
        let mut hub_client = client(&scratch, "hub", &hub).await;
        hub_client.create_library(key()).await.unwrap();
        scan(&scratch, &mut hub_client).await;
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        let desk_client = client(&scratch, "desk", &desk).await;
        join(&desk_client, &invite(&hub, &hub_client).await)
            .await
            .unwrap();
        let ids = song_ids(&desk_client.list_open(&songs()).await.unwrap().rows);

        // Each call is answered while the master is pulled, long before
        // the pull could end.
        let quick = PULL_PACE / 20;
        tokio::time::timeout(
            quick,
            desk_client.call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::Tracks { ids },
                start: None,
                context: None,
                shuffle: None,
            }),
        )
        .await
        .expect("play waited on the pull")
        .unwrap();
        let state = tokio::time::timeout(quick, desk_client.call::<PlaybackGet>(&NoParams {}))
            .await
            .expect("the player's state waited on the pull")
            .unwrap();
        assert_eq!(state.status, PlaybackStatus::Buffering);
        tokio::time::timeout(
            quick,
            desk_client.call::<PlaybackPause>(&PlaybackCommandParams {
                idempotency_key: key(),
            }),
        )
        .await
        .expect("a pause waited on the pull")
        .unwrap();

        // And the satellite stops while it is still under way.
        drop(desk_client);
        tokio::time::timeout(PULL_PACE / 20, desk.stop())
            .await
            .expect("the stop waited on the pull")
            .unwrap();
        drop(hub_client);
        hub.stop().await.unwrap();
    })
    .await;
}

/// What a satellite's cache of masters holds: a pull under way writes a
/// `.part` file there, and renames it once the master has crossed.
fn cached(scratch: &Scratch) -> Vec<PathBuf> {
    let cache = scratch.data_dir("desk").join("cache").join("audio");
    std::fs::read_dir(cache).map_or_else(
        |_| Vec::new(),
        |entries| entries.flatten().map(|entry| entry.path()).collect(),
    )
}

/// What a pull under way is writing in a satellite's cache.
fn partial(scratch: &Scratch) -> Vec<PathBuf> {
    cached(scratch)
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "part")
        })
        .collect()
}

/// A satellite's stop ends a pull under way and waits for it to remove
/// what it wrote: once the stop has returned, nothing of that server
/// writes to its data directory, which the same process may open again.
#[tokio::test(flavor = "multi_thread")]
async fn a_satellites_stop_ends_its_pull_and_leaves_nothing_behind() {
    bounded(async {
        let scratch = Scratch::new();
        // A hub that waits [`PULL_PACE`] before each part of a master.
        let hub = Server::start_run(
            HeldDir::take(scratch.data_dir("hub")).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            Bounds::default(),
            Run {
                audio_pace: Some(PULL_PACE),
                ..Run::default()
            },
        )
        .await
        .unwrap();
        let mut hub_client = client(&scratch, "hub", &hub).await;
        hub_client.create_library(key()).await.unwrap();
        scan(&scratch, &mut hub_client).await;
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        let desk_client = client(&scratch, "desk", &desk).await;
        join(&desk_client, &invite(&hub, &hub_client).await)
            .await
            .unwrap();
        let ids = song_ids(&desk_client.list_open(&songs()).await.unwrap().rows);
        desk_client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::Tracks { ids },
                start: None,
                context: None,
                shuffle: None,
            })
            .await
            .unwrap();
        while partial(&scratch).is_empty() {
            // sleep: between looks; the test's WAIT bounds them.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // The stop does not wait for the master to cross, and leaves no
        // part of it. Nothing at all is cached: a master that had crossed
        // before the stop would mean the pull was not under way.
        drop(desk_client);
        let stopping = std::time::Instant::now();
        desk.stop().await.unwrap();
        let took = stopping.elapsed();
        assert!(took < PULL_PACE / 20, "{took:?}");
        assert_eq!(cached(&scratch), Vec::<PathBuf>::new());

        // The directory is opened again in this process, and its stop, too,
        // leaves no part of a master, whether it began a pull or not.
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        desk.stop().await.unwrap();
        assert_eq!(partial(&scratch), Vec::<PathBuf>::new());
        drop(hub_client);
        hub.stop().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_plays_the_hubs_music_from_copies_it_pulls() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, _hub_client, _desk, mut desk_client) = linked_player(&scratch).await;
        let songs = desk_client.list_open(&songs()).await.unwrap();
        let ids: Vec<_> = songs
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Song(song) => Some(song.appearance_id),
                _ => None,
            })
            .collect();
        assert!(!ids.is_empty());
        // The play is answered before the first master is here, and the
        // player waits for it, buffering.
        let buffered =
            play_to_the_end(&mut desk_client, PlaySource::Tracks { ids: ids.clone() }).await;
        assert!(buffered);

        // Each master was pulled whole into the satellite's own cache.
        let kept = std::fs::read_dir(scratch.data_dir("desk").join("cache").join("audio"))
            .unwrap()
            .count();
        assert_eq!(kept, ids.len());
        // And the queue shows what the hub says of each track.
        let window = desk_client
            .call::<QueueWindow>(&QueueWindowParams {
                offset: 0,
                limit: None,
                order: None,
            })
            .await
            .unwrap();
        assert_eq!(window.entries.len(), ids.len());
        assert!(
            window
                .entries
                .iter()
                .all(|entry| entry.track.title.is_some() && entry.failed_reason.is_none()),
            "{window:?}"
        );

        // An album, and a view held at the hub, play as well.
        let Row::Song(song) = &songs.rows[0] else {
            panic!("not a song")
        };
        play_to_the_end(
            &mut desk_client,
            PlaySource::Release {
                id: song.release_id.unwrap(),
            },
        )
        .await;
        play_to_the_end(&mut desk_client, PlaySource::View { view: songs.view }).await;
        // A view of albums is not a list of songs, there as here.
        let albums = desk_client
            .list_open(&ListOpenParams {
                kind: ListKind::Albums,
                ..self::songs()
            })
            .await
            .unwrap();
        let refused = desk_client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::View { view: albums.view },
                start: None,
                context: None,
                shuffle: None,
            })
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Invalid), "{refused}");

        // A master is read by a satellite's link session alone: not by a
        // client of the hub, and not by anyone without a credential.
        let path = format!(
            "{}/{}/{}/1",
            route::AUDIO,
            song.appearance_id,
            song.appearance_id
        );
        let token = std::fs::read_to_string(scratch.data_dir("hub").join("credential")).unwrap();
        let local = ritornel_client::get(hub.endpoint(), &path, Some(&Credential::new(token)))
            .await
            .unwrap();
        assert_eq!(local.status, 403);
        let stranger = ritornel_client::get(hub.endpoint(), &path, None)
            .await
            .unwrap();
        assert_eq!(stranger.status, 401);
    })
    .await;
}

/// The listens the hub's record holds from the player of `satellite`, by
/// their IDs, read from its library as it stands.
fn at_hub(scratch: &Scratch, satellite: ritornel_client::contract::Id) -> Vec<Vec<u8>> {
    let db = rusqlite::Connection::open(scratch.data_dir("hub").join("library.db")).unwrap();
    let mut statement = db
        .prepare(
            "SELECT o.source_key FROM history_observation o
             JOIN history_endpoint e ON e.id = o.endpoint_id
             JOIN server s ON s.id = e.server_id
             WHERE e.kind = 'player' AND s.public_id = ?1 ORDER BY o.id",
        )
        .unwrap();
    statement
        .query_map([satellite.as_bytes()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The listens a satellite has staged and not yet seen the hub take: its
/// outbox, read from its `server.db`.
fn waiting(scratch: &Scratch) -> usize {
    let db = rusqlite::Connection::open(scratch.data_dir("desk").join("server.db")).unwrap();
    db.query_row("SELECT count(*) FROM outbox", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| usize::try_from(count).unwrap())
    .unwrap()
}

/// The listens a satellite has staged in all, delivered or not.
fn staged(scratch: &Scratch) -> usize {
    let db = rusqlite::Connection::open(scratch.data_dir("desk").join("server.db")).unwrap();
    db.query_row("SELECT count(*) FROM listen_staging", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| usize::try_from(count).unwrap())
    .unwrap()
}

/// Waits until the satellite's outbox is empty.
async fn delivered(scratch: &Scratch) {
    while waiting(scratch) > 0 {
        // sleep: between looks; the test's WAIT bounds them.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn song_ids(rows: &[Row]) -> Vec<ritornel_client::contract::Id> {
    rows.iter()
        .filter_map(|row| match row {
            Row::Song(song) => Some(song.appearance_id),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellites_listens_reach_the_hubs_record_once() {
    bounded(async {
        let scratch = Scratch::new();
        let (_hub, _hub_client, desk, mut desk_client) = linked_player(&scratch).await;
        let ids = song_ids(&desk_client.list_open(&songs()).await.unwrap().rows);
        play_to_the_end(&mut desk_client, PlaySource::Tracks { ids: ids.clone() }).await;
        delivered(&scratch).await;
        let taken = at_hub(&scratch, desk.server_id());
        assert_eq!(taken.len(), ids.len());
        // The satellite keeps nothing once the hub has them.
        assert_eq!(staged(&scratch), 0);

        // Played again, they are new listens, and each is taken once.
        play_to_the_end(&mut desk_client, PlaySource::Tracks { ids: ids.clone() }).await;
        delivered(&scratch).await;
        let mut taken = at_hub(&scratch, desk.server_id());
        assert_eq!(taken.len(), 2 * ids.len());
        taken.sort();
        taken.dedup();
        assert_eq!(taken.len(), 2 * ids.len());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn listens_wait_while_the_hub_is_away_and_arrive_once_when_it_is_back() {
    bounded(async {
        let scratch = Scratch::new();
        // In real time: a track lasts half a second.
        let (hub, hub_client, desk, mut desk_client) = linked_player_at(&scratch, 1).await;
        let ids = song_ids(&desk_client.list_open(&songs()).await.unwrap().rows);

        // The hub goes while the track plays from the copy pulled for it.
        desk_client.subscribe(&[Topic::Playback]).await.unwrap();
        desk_client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::Tracks { ids: vec![ids[0]] },
                start: None,
                context: None,
                shuffle: None,
            })
            .await
            .unwrap();
        // It answers at once, and plays once its master is here.
        loop {
            if let Event::Playback(event) = desk_client.next_event().await.unwrap()
                && event.state.status == PlaybackStatus::Playing
            {
                break;
            }
        }
        let port = hub.endpoint().port();
        drop(hub_client);
        hub.stop().await.unwrap();
        loop {
            if let Event::Playback(event) = desk_client.next_event().await.unwrap()
                && event.state.status == PlaybackStatus::Stopped
            {
                break;
            }
        }
        // The track played to its end, and its listen waits.
        assert_eq!(waiting(&scratch), 1);

        // The hub comes back where it was, and takes the listen once.
        let _hub = start_on(&scratch, "hub", Listen::on_port(port)).await;
        delivered(&scratch).await;
        assert_eq!(at_hub(&scratch, desk.server_id()).len(), 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listen_sent_again_is_taken_once_and_only_from_its_own_player() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, _hub_client, desk, mut desk_client) = linked_player(&scratch).await;
        let ids = song_ids(&desk_client.list_open(&songs()).await.unwrap().rows);
        play_to_the_end(&mut desk_client, PlaySource::Tracks { ids: vec![ids[0]] }).await;
        delivered(&scratch).await;
        // The listen as the hub stored it, as a satellite whose
        // acknowledgement was lost would send it again.
        let hub_db =
            rusqlite::Connection::open(scratch.data_dir("hub").join("library.db")).unwrap();
        let payload: String = hub_db
            .query_row(
                "SELECT payload_json FROM history_observation WHERE kind = 'listen' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let record: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let listen_id: ritornel_client::contract::Id =
            record["listenId"].as_str().unwrap().parse().unwrap();

        let token = {
            let db =
                rusqlite::Connection::open(scratch.data_dir("desk").join("server.db")).unwrap();
            db.query_row("SELECT token FROM hub_link", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
        };
        let linked = Client::connect(hub.endpoint(), &Credential::new(&token))
            .await
            .unwrap();
        linked
            .hello(&HelloParams {
                name: "desk".to_owned(),
                kind: ClientKind::Server,
                version: "1".to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await
            .unwrap();
        let send = |record: serde_json::Value, id| LinkListenParams {
            idempotency_key: id,
            record,
            sent_at_ms: 1,
            sent_mono_ms: 1,
            sent_boot: id,
        };
        let again = linked
            .call::<LinkListen>(&send(record.clone(), listen_id))
            .await
            .unwrap();
        assert_eq!(again.taken, ListenTaken::Duplicate);
        assert_eq!(at_hub(&scratch, desk.server_id()).len(), 1);

        // A listen of another server's player is not this satellite's to
        // report.
        let mut forged = record.clone();
        forged["playedOnServer"] = serde_json::json!(hub.server_id().to_string());
        let refused = linked
            .call::<LinkListen>(&send(forged, listen_id))
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Forbidden), "{refused}");
        // A number no clock reaches is refused before the record sees it,
        // and the hub takes listens as before.
        let mut wild = record.clone();
        wild["startedAtMs"] = serde_json::json!(i64::MAX);
        let refused = linked
            .call::<LinkListen>(&send(wild, listen_id))
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Invalid), "{refused}");
        let again = linked
            .call::<LinkListen>(&send(record.clone(), listen_id))
            .await
            .unwrap();
        assert_eq!(again.taken, ListenTaken::Duplicate);
        // Nor does a client of the hub report listens at all.
        let hub_client = client(&scratch, "hub", &hub).await;
        let refused = hub_client
            .call::<LinkListen>(&send(record, listen_id))
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Forbidden), "{refused}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_pings_its_hub_and_only_the_hubs_own_machine_says_when_it_stays_awake() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, _desk, _desk_client) = linked(&scratch).await;
        let token = {
            let db =
                rusqlite::Connection::open(scratch.data_dir("desk").join("server.db")).unwrap();
            db.query_row("SELECT token FROM hub_link", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
        };
        let linked = Client::connect(hub.endpoint(), &Credential::new(&token))
            .await
            .unwrap();
        linked
            .hello(&HelloParams {
                name: "desk".to_owned(),
                kind: ClientKind::Server,
                version: "1".to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await
            .unwrap();
        // Whether the hub is kept awake is the system's to grant, so only
        // the answer is looked for here; the rule has tests of its own.
        linked
            .call::<LinkPing>(&LinkPingParams { playing: true })
            .await
            .unwrap();
        let refused = hub_client
            .call::<LinkPing>(&LinkPingParams { playing: true })
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Forbidden), "{refused}");

        // While active, for 30 minutes, until someone says otherwise.
        let status = hub_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(status.awake, Awake::default());
        let settings = hub_client
            .call::<ServerSettingsCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(settings.awake.value, Awake::default());
        assert_eq!(settings.awake.set_by, SetBy::Default);
        let to = |awake| SetServerSettingsParams {
            idempotency_key: ritornel_client::idempotency_key().unwrap(),
            awake: Some(awake),
        };
        let set = hub_client
            .call::<ServerSetSettings>(&to(Awake::Never))
            .await
            .unwrap();
        assert_eq!(set.settings.awake.value, Awake::Never);
        assert_eq!(set.settings.awake.set_by, SetBy::Call);
        // A satellite does not decide when its hub sleeps, nor read how
        // the hub is set.
        let refused = linked
            .call::<ServerSetSettings>(&to(Awake::Always))
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Forbidden), "{refused}");
        let refused = linked
            .call::<ServerSettingsCall>(&NoParams {})
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Forbidden), "{refused}");
        let refused = hub_client
            .call::<ServerSetSettings>(&to(Awake::WhileActive { minutes: 1_000_000 }))
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Invalid), "{refused}");
        let nothing = SetServerSettingsParams {
            awake: None,
            ..to(Awake::Never)
        };
        let refused = hub_client
            .call::<ServerSetSettings>(&nothing)
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Invalid), "{refused}");
        let status = hub_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(status.awake, Awake::Never);
        assert!(!status.kept_awake);
        // A command sent again changes nothing, and its ID is not taken
        // for another.
        let once = to(Awake::Always);
        hub_client.call::<ServerSetSettings>(&once).await.unwrap();
        let other = SetServerSettingsParams {
            awake: Some(Awake::Never),
            ..once
        };
        let refused = hub_client
            .call::<ServerSetSettings>(&other)
            .await
            .unwrap_err();
        assert_eq!(
            code(&refused),
            Some(ErrorCode::IdempotencyMismatch),
            "{refused}"
        );
        let again = hub_client.call::<ServerSetSettings>(&once).await.unwrap();
        assert_eq!(again.settings.awake.value, Awake::Always);
        // Setting the default is a choice, and is kept as one.
        let set = hub_client
            .call::<ServerSetSettings>(&to(Awake::default()))
            .await
            .unwrap();
        assert_eq!(set.settings.awake.value, Awake::default());
        assert_eq!(set.settings.awake.set_by, SetBy::Call);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_answers_with_its_own_settings_and_changes_only_its_own() {
    bounded(async {
        let scratch = Scratch::new();
        let (_hub, hub_client, desk, desk_client) = linked(&scratch).await;
        let desk_settings = desk_client
            .call::<ServerSettingsCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(desk_settings.name.value, desk.name());

        let set = desk_client
            .call::<ServerSetSettings>(&SetServerSettingsParams {
                idempotency_key: key(),
                awake: Some(Awake::Always),
            })
            .await
            .unwrap();
        assert_eq!(set.settings.awake.value, Awake::Always);
        assert_eq!(set.settings.name.value, desk.name());
        let hub_settings = hub_client
            .call::<ServerSettingsCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(hub_settings.awake.value, Awake::default());
        assert_eq!(hub_settings.awake.set_by, SetBy::Default);

        // Kept with the satellite, across a restart.
        drop(desk_client);
        desk.stop().await.unwrap();
        let desk = start(&scratch, "desk").await;
        let desk_client = client(&scratch, "desk", &desk).await;
        let desk_settings = desk_client
            .call::<ServerSettingsCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(desk_settings.awake.value, Awake::Always);
        assert_eq!(desk_settings.awake.set_by, SetBy::Call);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_renamed_after_its_computer_is_listed_by_its_new_name() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, desk, desk_client) = linked(&scratch).await;
        let desk_id = desk.server_id();
        let before = hub_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_ne!(before.satellites[0].name, "Studio PC");

        // Started again where an older build had named it after the
        // computer otherwise, it takes the computer's name as read now.
        drop(desk_client);
        desk.stop().await.unwrap();
        let desk = Server::start_run(
            HeldDir::take(scratch.data_dir("desk")).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            Bounds::default(),
            Run {
                computer_name: Some("Studio PC".to_owned()),
                legacy_name: Some(before.satellites[0].name.clone()),
                ..Run::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(desk.name(), "Studio PC");
        // Its next call to the hub says hello under that name.
        let desk_client = client(&scratch, "desk", &desk).await;
        desk_client
            .call::<LibraryStats>(&NoParams {})
            .await
            .unwrap();
        let after = hub_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(after.satellites[0].name, "Studio PC");
        let registry: String =
            rusqlite::Connection::open(scratch.data_dir("hub").join("library.db"))
                .unwrap()
                .query_row(
                    "SELECT name FROM server WHERE public_id = ?1",
                    [desk_id.as_bytes()],
                    |row| row.get(0),
                )
                .unwrap();
        assert_eq!(registry, "Studio PC");
        drop(hub);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_takes_its_computers_name_and_keeps_one_chosen() {
    bounded(async {
        let scratch = Scratch::new();
        let named = |name: &str| Run {
            computer_name: Some(name.to_owned()),
            ..Run::default()
        };
        let start_named = async |name: &str| {
            Server::start_run(
                HeldDir::take(scratch.data_dir("desk")).unwrap(),
                Listen::on_port(0),
                Arc::new(SystemClock),
                Arc::new(OsEntropy),
                Bounds::default(),
                named(name),
            )
            .await
            .unwrap()
        };
        let desk = start_named("Studio Mac").await;
        assert_eq!(desk.name(), "Studio Mac");
        desk.stop().await.unwrap();
        // The computer renamed later does not rename a server that already
        // has a name of its own.
        let desk = start_named("Other Name").await;
        assert_eq!(desk.name(), "Studio Mac");
        desk.stop().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_setting_wins_over_the_computers_name_and_the_stored_one() {
    bounded(async {
        let scratch = Scratch::new();
        let start = async |dir: &str, computer: Option<&str>, setting: Option<&str>| {
            let run = Run {
                computer_name: computer.map(str::to_owned),
                name: setting.map(str::to_owned),
                ..Run::default()
            };
            let server = Server::start_run(
                HeldDir::take(scratch.data_dir(dir)).unwrap(),
                Listen::on_port(0),
                Arc::new(SystemClock),
                Arc::new(OsEntropy),
                Bounds::default(),
                run,
            )
            .await
            .unwrap();
            let name = server.name();
            server.stop().await.unwrap();
            name
        };
        // On a first start the setting wins over the computer's name.
        let porch = Some("Porch Speakers");
        assert_eq!(
            start("porch", Some("pi-zero"), porch).await,
            "Porch Speakers"
        );
        // Left out later, the name it gave stands.
        assert_eq!(
            start("porch", Some("pi-zero"), None).await,
            "Porch Speakers"
        );
        // Given again, it replaces a name the computer gave.
        assert_eq!(start("desk", Some("Studio Mac"), None).await, "Studio Mac");
        assert_eq!(
            start("desk", Some("Studio Mac"), porch).await,
            "Porch Speakers"
        );
        // A computer with no name leaves the default until it has one.
        assert_eq!(start("attic", None, None).await, "Ritornel server");
        assert_eq!(start("attic", Some("Attic Box"), None).await, "Attic Box");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellites_clients_hear_when_the_hub_goes_and_comes_back() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, _desk, mut desk_client) = linked(&scratch).await;
        desk_client
            .call::<LibraryStats>(&NoParams {})
            .await
            .unwrap();
        desk_client.subscribe(&[Topic::Servers]).await.unwrap();
        let port = hub.endpoint().port();
        drop(hub_client);
        hub.stop().await.unwrap();
        // Nothing is asked of the hub: the satellite finds out by itself,
        // and says so on its own status.
        loop {
            if let Event::Servers(_) = desk_client.next_event().await.unwrap()
                && desk_client.status().await.unwrap().hub.unwrap().answers == Some(false)
            {
                break;
            }
        }
        let _hub = start_on(&scratch, "hub", Listen::on_port(port)).await;
        loop {
            if let Event::Servers(_) = desk_client.next_event().await.unwrap()
                && desk_client.status().await.unwrap().hub.unwrap().answers == Some(true)
            {
                break;
            }
        }
    })
    .await;
}

/// A relay between a satellite and its hub that can hold back what the
/// hub sends while it still passes on what the satellite sends, as a hub
/// does that has stopped answering without closing its connections.
struct Relay {
    address: std::net::SocketAddr,
    holding: Arc<std::sync::atomic::AtomicBool>,
    /// Everything the hub has sent through it, on every connection.
    heard: Arc<std::sync::Mutex<Vec<u8>>>,
}

impl Relay {
    async fn to(hub: std::net::SocketAddr) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let holding = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let held = Arc::clone(&holding);
        let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hearing = Arc::clone(&heard);
        tokio::spawn(async move {
            while let Ok((satellite, _)) = listener.accept().await {
                let Ok(hub) = tokio::net::TcpStream::connect(hub).await else {
                    continue;
                };
                let (mut from_satellite, mut to_satellite) = satellite.into_split();
                let (mut from_hub, mut to_hub) = hub.into_split();
                tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut from_satellite, &mut to_hub).await;
                });
                let held = Arc::clone(&held);
                let hearing = Arc::clone(&hearing);
                tokio::spawn(async move {
                    let mut chunk = vec![0; 16 * 1024];
                    while let Ok(read @ 1..) = from_hub.read(&mut chunk).await {
                        while held.load(Ordering::SeqCst) {
                            // sleep: the relay holds what the hub sends for as
                            // long as the test says.
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        hearing.lock().unwrap().extend_from_slice(&chunk[..read]);
                        if to_satellite.write_all(&chunk[..read]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self {
            address,
            holding,
            heard,
        }
    }

    /// How many of the hub's library events have come through it.
    fn library_events(&self) -> usize {
        let heard = self.heard.lock().unwrap();
        let topic = br#""topic":"library""#;
        heard
            .windows(topic.len())
            .filter(|seen| seen == topic)
            .count()
    }

    fn hold(&self, holding: bool) {
        self.holding.store(holding, Ordering::SeqCst);
    }
}

/// A satellite whose hub stops answering stops within the time its stop
/// gives the hub, however long a call would wait, and the listens whose
/// answers it never had are each taken once, from the next run.
#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_stops_in_time_while_its_hub_does_not_answer_and_its_listens_arrive_once() {
    bounded(async {
        let scratch = Scratch::new();
        let hub = start(&scratch, "hub").await;
        let mut hub_client = client(&scratch, "hub", &hub).await;
        hub_client.create_library(key()).await.unwrap();
        scan(&scratch, &mut hub_client).await;
        let relay = Relay::to(hub.endpoint()).await;
        let grace = Duration::from_secs(1);
        // In real time: a track lasts half a second.
        let desk = Server::start_run(
            HeldDir::take(scratch.data_dir("desk")).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            // A call to the hub waits a minute here, so that a stop that
            // waited for one would be plain to see.
            Bounds {
                stop_grace: grace,
                hub_call_wait: Duration::from_secs(60),
                ..Bounds::default()
            },
            Run {
                playback: PlaybackRun {
                    output: Some(Output::Null { speed: 1 }),
                    ..PlaybackRun::default()
                },
                ..Run::default()
            },
        )
        .await
        .unwrap();
        let mut desk_client = client(&scratch, "desk", &desk).await;
        let link = hub_client
            .call::<LinkInvite>(&LinkInviteParams {
                routes: vec![relay.address.to_string()],
            })
            .await
            .unwrap()
            .link;
        join(&desk_client, &link).await.unwrap();
        let ids = song_ids(&desk_client.list_open(&songs()).await.unwrap().rows);

        // The hub stops answering while the first of two plays of a song
        // goes on. The second is ready by then, and the first's listen is
        // sent after.
        desk_client.subscribe(&[Topic::Playback]).await.unwrap();
        desk_client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::Tracks {
                    ids: vec![ids[0], ids[0]],
                },
                start: None,
                context: None,
                shuffle: None,
            })
            .await
            .unwrap();
        let mut held = false;
        loop {
            if let Event::Playback(event) = desk_client.next_event().await.unwrap() {
                if event.state.status == PlaybackStatus::Playing && !held {
                    relay.hold(true);
                    held = true;
                }
                if held && event.state.status == PlaybackStatus::Stopped {
                    break;
                }
            }
        }
        // The hub has taken a listen whose answer is held back: one it has
        // that the satellite still waits to hear of. On a slow machine the
        // first may have been answered before the hold, but not the second.
        let id = desk.server_id();
        while at_hub(&scratch, id).len() + waiting(&scratch) <= 2 {
            // sleep: between looks; the test's WAIT bounds them.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // A call waits on the hub for a minute; the stop gives it one
        // second, and spends that at most twice. A stop that waited for the
        // call would take the minute.
        drop(desk_client);
        let stopping = std::time::Instant::now();
        desk.stop().await.unwrap();
        let took = stopping.elapsed();
        assert!(took < Duration::from_secs(30), "{took:?}");
        assert!(waiting(&scratch) >= 1);

        // The hub answers again, and the next run sends what waits again:
        // each listen is taken once.
        relay.hold(false);
        let _desk = start_playing_with(&scratch, "desk", 100, None).await;
        delivered(&scratch).await;
        assert_eq!(at_hub(&scratch, id).len(), 2);
        assert_eq!(staged(&scratch), 0);
    })
    .await;
}

/// Rates the first song at the hub, as any client of the hub may.
async fn rate(hub_client: &Client, rating: u32) {
    let Row::Song(song) = hub_client.list_open(&songs()).await.unwrap().rows[0].clone() else {
        panic!("not a song");
    };
    hub_client
        .call::<RatingsSet>(&RatingSetParams {
            idempotency_key: key(),
            recording_id: song.recording_id,
            rating: Some(rating),
        })
        .await
        .unwrap();
}

/// Waits for a `servers` event after which the satellite says its hub
/// answers, or does not.
async fn hub_answers(desk_client: &mut Client, answers: bool) {
    loop {
        if let Event::Servers(_) = desk_client.next_event().await.unwrap()
            && desk_client.status().await.unwrap().hub.unwrap().answers == Some(answers)
        {
            return;
        }
    }
}

/// A client that only listens hears the hub's events again once the hub
/// is back from a restart, on the same session and without a call to the
/// hub in between.
#[tokio::test(flavor = "multi_thread")]
async fn a_satellites_client_hears_the_hubs_events_again_after_the_hub_restarts() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, _desk, mut desk_client) = linked(&scratch).await;
        desk_client
            .subscribe(&[Topic::Library, Topic::Servers])
            .await
            .unwrap();
        rate(&hub_client, 60).await;
        loop {
            if let Event::Library(_) = desk_client.next_event().await.unwrap() {
                break;
            }
        }

        let port = hub.endpoint().port();
        drop(hub_client);
        hub.stop().await.unwrap();
        hub_answers(&mut desk_client, false).await;
        let hub = start_on(&scratch, "hub", Listen::on_port(port)).await;
        let hub_client = client(&scratch, "hub", &hub).await;
        hub_answers(&mut desk_client, true).await;

        rate(&hub_client, 80).await;
        loop {
            if let Event::Library(_) = desk_client.next_event().await.unwrap() {
                break;
            }
        }
    })
    .await;
}

/// A client that subscribes while the hub is away hears the hub's events
/// once it is back, without a call to the hub in between.
#[tokio::test(flavor = "multi_thread")]
async fn a_satellites_client_that_subscribes_while_the_hub_is_away_hears_it_when_back() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, _desk, mut desk_client) = linked(&scratch).await;
        desk_client.subscribe(&[Topic::Servers]).await.unwrap();
        let port = hub.endpoint().port();
        drop(hub_client);
        hub.stop().await.unwrap();
        hub_answers(&mut desk_client, false).await;

        desk_client.subscribe(&[Topic::Library]).await.unwrap();
        let hub = start_on(&scratch, "hub", Listen::on_port(port)).await;
        let hub_client = client(&scratch, "hub", &hub).await;
        hub_answers(&mut desk_client, true).await;

        rate(&hub_client, 80).await;
        loop {
            if let Event::Library(_) = desk_client.next_event().await.unwrap() {
                break;
            }
        }
    })
    .await;
}

/// A client that unsubscribes from the library's events is no longer sent
/// them by the hub, not only no longer handed them by the satellite.
#[tokio::test(flavor = "multi_thread")]
async fn a_satellites_client_that_unsubscribes_is_unsubscribed_at_the_hub() {
    bounded(async {
        let scratch = Scratch::new();
        let (hub, mut hub_client, _desk, desk_client) = two(&scratch).await;
        scan(&scratch, &mut hub_client).await;
        let relay = Relay::to(hub.endpoint()).await;
        let link = hub_client
            .call::<LinkInvite>(&LinkInviteParams {
                routes: vec![relay.address.to_string()],
            })
            .await
            .unwrap()
            .link;
        join(&desk_client, &link).await.unwrap();
        let mut desk_client = desk_client;
        desk_client.subscribe(&[Topic::Library]).await.unwrap();
        rate(&hub_client, 60).await;
        loop {
            if let Event::Library(_) = desk_client.next_event().await.unwrap() {
                break;
            }
        }
        let before = relay.library_events();
        assert!(before >= 1);

        desk_client.unsubscribe(&[Topic::Library]).await.unwrap();
        rate(&hub_client, 80).await;
        // The hub answers a session's calls on its connection after the
        // events it queued for it before: once this is answered, an event
        // the hub sent for the change has come through the relay.
        desk_client
            .call::<LibraryStats>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(relay.library_events(), before);
    })
    .await;
}
