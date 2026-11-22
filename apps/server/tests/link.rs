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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ritornel_client::contract::methods::{
    LibraryStats, LinkInvite, LinkJoin, LinkListen, LinkPing, LinkSetAwake, LinkStatusCall,
    PlaybackPlay, QueueWindow, RatingsSet, SourcesAdd,
};
use ritornel_client::contract::{
    Awake, AwakeParams, CONTRACT_VERSION, ClientKind, ErrorCode, Event, HelloParams, InviteLink,
    LinkInviteParams, LinkJoinParams, LinkListenParams, LinkPingParams, LinkRequest, LinkStatus,
    ListKind, ListOpenParams, ListenTaken, NoParams, PlaySource, PlaybackPlayParams,
    PlaybackStatus, Query, QueueWindowParams, RatingSetParams, Row, ServerRole, SourcesAddParams,
    Topic, Window, capability, hex, link_request_proof, route,
};
use ritornel_client::{Client, Credential};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Output, PlaybackRun, Run, Server};

/// How long a test may take before it is failed.
const WAIT: Duration = Duration::from_secs(60);

async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(WAIT, test)
        .await
        .expect("the test did not finish in time");
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ritornel-link-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
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

/// A server with a player on the null output, `speed` times real time.
async fn start_playing_at(scratch: &Scratch, name: &str, speed: u32) -> Server {
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
        let started = std::time::Instant::now();
        let status = join(&desk_client, &named.to_string()).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        let hub_status = status.hub.unwrap();
        assert_eq!(hub_status.server_id, hub.server_id());
        assert_eq!(hub_status.routes, named.routes);
        let stats = desk_client
            .call::<LibraryStats>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(stats.artists, 0);
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
        let hub = start_on(&scratch, "hub", Listen::on_port(port)).await;
        loop {
            match desk_client.call::<LibraryStats>(&NoParams {}).await {
                Ok(_) => break,
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
    let hub = start(scratch, "hub").await;
    let mut hub_client = client(scratch, "hub", &hub).await;
    hub_client.create_library(key()).await.unwrap();
    scan(scratch, &mut hub_client).await;
    let desk = start_playing_at(scratch, "desk", speed).await;
    let desk_client = client(scratch, "desk", &desk).await;
    join(&desk_client, &invite(&hub, &hub_client).await)
        .await
        .unwrap();
    (hub, hub_client, desk, desk_client)
}

/// Plays `source` on the satellite and waits until the player has stopped
/// at the end of it.
async fn play_to_the_end(client: &mut Client, source: PlaySource) {
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
    let mut played = false;
    loop {
        if let Event::Playback(event) = client.next_event().await.unwrap() {
            played |= event.state.status == PlaybackStatus::Playing;
            if played && event.state.status == PlaybackStatus::Stopped {
                break;
            }
        }
    }
    client.unsubscribe(&[Topic::Playback]).await.unwrap();
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
        play_to_the_end(&mut desk_client, PlaySource::Tracks { ids: ids.clone() }).await;

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
        let port = hub.endpoint().port();
        drop(hub_client);
        hub.stop().await.unwrap();
        let mut played = false;
        loop {
            if let Event::Playback(event) = desk_client.next_event().await.unwrap() {
                played |= event.state.status == PlaybackStatus::Playing;
                if played && event.state.status == PlaybackStatus::Stopped {
                    break;
                }
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

        // While active, for 30 minutes, until the owner says otherwise.
        let status = hub_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(status.awake, Awake::default());
        let never = |awake| AwakeParams {
            idempotency_key: ritornel_client::idempotency_key().unwrap(),
            awake,
        };
        let set = hub_client
            .call::<LinkSetAwake>(&never(Awake::Never))
            .await
            .unwrap();
        assert_eq!(set.link.awake, Awake::Never);
        assert!(!set.link.kept_awake);
        // A satellite does not decide when its hub sleeps.
        let refused = linked
            .call::<LinkSetAwake>(&never(Awake::Always))
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Forbidden), "{refused}");
        let refused = hub_client
            .call::<LinkSetAwake>(&never(Awake::WhileActive { minutes: 1_000_000 }))
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::Invalid), "{refused}");
        let status = hub_client
            .call::<LinkStatusCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(status.awake, Awake::Never);
        // A command sent again changes nothing, and its ID is not taken
        // for another.
        let once = never(Awake::Always);
        hub_client.call::<LinkSetAwake>(&once).await.unwrap();
        let other = AwakeParams {
            awake: Awake::Never,
            ..once
        };
        assert!(hub_client.call::<LinkSetAwake>(&other).await.is_err());
        let again = hub_client.call::<LinkSetAwake>(&once).await.unwrap();
        assert_eq!(again.link.awake, Awake::Always);
        hub_client
            .call::<LinkSetAwake>(&never(Awake::default()))
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_takes_its_computers_name_and_keeps_one_chosen() {
    bounded(async {
        let scratch = Scratch::new();
        let named = |name: &str| Run {
            name: Some(name.to_owned()),
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
