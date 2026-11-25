//! The link between a hub and a satellite: two real servers inside the
//! test process, each on a data directory of its own and a port the system
//! picks, joined over loopback as two machines are over their network.

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
    PlaybackGetParams, PlaybackPlayParams, PlaybackStatus, Query, QueueWindowParams,
    RatingSetParams, Row, ServerRole, SetBy, SetServerSettingsParams, SourcesAddParams, Topic,
    Window, capability, hex, link_request_proof, route,
};
use ritornel_client::{Client, Credential};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Output, PlaybackRun, Run, Server};

use super::support::{self, album, bounded, key};

/// How long a slow hub waits before each part of a master it sends: far
/// longer than anything a test does while the pull is under way, so that
/// none can outlast it however slow the machine.
const PULL_PACE: Duration = Duration::from_secs(600);

struct Scratch(support::Scratch);

impl Scratch {
    fn new() -> Self {
        Self(support::Scratch::new("ritornel-link-"))
    }

    fn data_dir(&self, name: &str) -> PathBuf {
        self.0.0.join(name)
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
    start_with_run(
        scratch,
        name,
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
}

async fn start_with_run(scratch: &Scratch, name: &str, run: Run) -> Server {
    Server::start_run(
        HeldDir::take(scratch.data_dir(name)).unwrap(),
        Listen::on_port(0),
        Arc::new(SystemClock),
        Arc::new(OsEntropy),
        Bounds::default(),
        run,
    )
    .await
    .unwrap()
}

/// Adds the album to the hub's library and scans it, and waits for the
/// scan to end.
async fn scan(scratch: &Scratch, hub: &mut Client) {
    scan_folder(hub, &album(&scratch.0)).await;
}

async fn scan_folder(hub: &mut Client, folder: &Path) {
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
            presence: None,
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
                presence: None,
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
        for method in [
            "sources.add",
            "server.backup",
            "history.export",
            "transcode.start",
            "transcode.prune",
        ] {
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
        for method in ["transcode.start", "transcode.prune"] {
            assert_eq!(
                code(
                    &desk_client
                        .call_raw(method, serde_json::json!({}))
                        .await
                        .unwrap_err()
                ),
                Some(ErrorCode::Unsupported)
            );
        }
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
        let hub = restart_hub(&scratch, port).await;
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

async fn restart_hub(scratch: &Scratch, port: u16) -> Server {
    // The port was let go a moment ago, and another test's server, or
    // the system, may hold it a little longer: it is tried until free.
    loop {
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
    }
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
            player: None,
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
            let folder = scratch.0.0.join("music").join(folder);
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
                player: None,
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
                player: None,
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
        let state = tokio::time::timeout(
            quick,
            desk_client.call::<PlaybackGet>(&PlaybackGetParams::default()),
        )
        .await
        .expect("the player's state waited on the pull")
        .unwrap();
        assert_eq!(state.status, PlaybackStatus::Buffering);
        tokio::time::timeout(
            quick,
            desk_client.call::<PlaybackPause>(&PlaybackCommandParams {
                player: None,
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
                player: None,
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
                player: None,
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
                player: None,
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
                player: None,
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
                presence: None,
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
                presence: None,
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
            .call::<LinkPing>(&LinkPingParams {
                playing: true,
                player: None,
            })
            .await
            .unwrap();
        let refused = hub_client
            .call::<LinkPing>(&LinkPingParams {
                playing: true,
                player: None,
            })
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
                player: None,
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

#[tokio::test(flavor = "multi_thread")]
async fn cache_routes_require_a_link_and_serve_the_exact_pinned_bytes() {
    use ritornel_engine::playback::FileFacts;
    use ritornel_engine::transcode::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, desk, desk_client) = two(&scratch).await;
        join(&desk_client, &invite(&hub, &hub_client).await)
            .await
            .unwrap();
        let link: String = rusqlite::Connection::open(scratch.data_dir("desk").join("server.db"))
            .unwrap()
            .query_row("SELECT token FROM hub_link", [], |r| r.get(0))
            .unwrap();
        let local = std::fs::read_to_string(scratch.data_dir("hub").join("credential")).unwrap();
        let endpoint = hub.endpoint();
        let fetch = async |method: &str,
                           path: &str,
                           token: Option<&str>,
                           range: Option<&str>,
                           validator: Option<&str>| {
            let mut req =
                format!("{method} {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n");
            if let Some(token) = token {
                req.push_str(&format!("Authorization: Bearer {token}\r\n"));
            }
            if let Some(range) = range {
                req.push_str(&format!("Range: {range}\r\n"));
            }
            if let Some(tag) = validator {
                req.push_str(&format!("If-Range: {tag}\r\n"));
            }
            req.push_str("\r\n");
            let mut conn = tokio::net::TcpStream::connect(endpoint).await.unwrap();
            conn.write_all(req.as_bytes()).await.unwrap();
            let mut bytes = Vec::new();
            conn.read_to_end(&mut bytes).await.unwrap();
            let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let headers = std::str::from_utf8(&bytes[..end])
                .unwrap()
                .to_ascii_lowercase();
            let status = headers
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u16>()
                .unwrap();
            (status, headers, bytes[end + 4..].to_vec())
        };

        // Seed both cache layers with the production pool, independent of PATH.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio/a.flac");
        let master = Master {
            asset: AudioAsset {
                scheme: 1,
                hash: [42; 32].into(),
            },
            facts: FileFacts::of(&path.metadata().unwrap()),
            path,
        };
        let jobs = ritornel_engine::ServerStore::open_in_memory(
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
        .unwrap();
        let cancel = jobs.job_cancel(1, Arc::default()).unwrap();
        let items = vec![
            BatchItem::Derivative {
                master: master.clone(),
                encoding: Encoding {
                    backend: Arc::new(FakeEncoder),
                    id: FakeEncoder.probe().unwrap().unwrap(),
                    profile: Profile {
                        target: Target::Mp3 { mode: Mp3::V2 },
                        dsp: Dsp::default(),
                    },
                    gain_tags: GainTags::default(),
                },
            },
            BatchItem::Package {
                master,
                audio: PackageAudio::PassThrough(Codec::Flac),
                packaging: Arc::new(Packaging {
                    tags: Tags::default(),
                    cover: None,
                    id3v1: false,
                }),
            },
        ];
        let batch = hub
            .transcode_pool()
            .ensure_batch(items, &cancel, |_| {})
            .await
            .unwrap();
        for (entry, (layer, mime)) in batch
            .entries()
            .zip([("derivative", "audio/mpeg"), ("packaged", "audio/flac")])
        {
            let path = format!("/v1/cache/{layer}/{}", entry.key());
            let content = std::fs::read(entry.path()).unwrap();
            let size = content.len();
            assert_eq!(fetch("GET", &path, None, None, None).await.0, 401);
            assert_eq!(
                fetch("GET", &path, Some(local.trim()), None, None).await.0,
                403
            );
            for method in ["GET", "HEAD"] {
                let (status, headers, body) = fetch(method, &path, Some(&link), None, None).await;
                assert_eq!(status, 200);
                assert!(
                    headers
                        .lines()
                        .any(|l| l == format!("content-length: {size}"))
                );
                assert!(headers.contains(&format!("content-type: {mime}")));
                assert!(headers.contains(&format!("etag: \"{}\"", entry.file_hash)));
                assert_eq!(
                    body.as_slice(),
                    if method == "GET" {
                        content.as_slice()
                    } else {
                        &[]
                    }
                );
            }
            for (range, start, end) in [
                ("bytes=1-8", 1, 8),
                ("bytes=9-", 9, size - 1),
                ("bytes=-5", size - 5, size - 1),
            ] {
                let (status, headers, body) =
                    fetch("GET", &path, Some(&link), Some(range), None).await;
                assert_eq!(status, 206);
                assert!(headers.contains(&format!("content-range: bytes {start}-{end}/{size}")));
                assert!(
                    headers
                        .lines()
                        .any(|l| l == format!("content-length: {}", end - start + 1))
                );
                assert_eq!(body, content[start..=end]);
            }
            let etag = format!("\"{}\"", entry.file_hash);
            for (validator, status, bytes) in [
                (etag.as_str(), 206, &content[..2]),
                ("\"stale\"", 200, content.as_slice()),
            ] {
                let got = fetch(
                    "GET",
                    &path,
                    Some(&link),
                    Some("bytes=0-1"),
                    Some(validator),
                )
                .await;
                assert_eq!((got.0, got.2.as_slice()), (status, bytes));
            }
            let (status, headers, _) =
                fetch("GET", &path, Some(&link), Some("bytes=4-2"), None).await;
            assert_eq!(status, 416);
            assert!(headers.contains(&format!("content-range: bytes */{size}")));
            for bad in ["abc", "%2E%2E%2Fsecret", &"g".repeat(64)] {
                let path = format!("/v1/cache/{layer}/{bad}");
                assert_eq!(fetch("GET", &path, Some(&link), None, None).await.0, 400);
            }
            let missing = format!("/v1/cache/{layer}/{}", "0".repeat(64));
            assert_eq!(fetch("GET", &missing, Some(&link), None, None).await.0, 404);
        }
        drop(batch);
        drop((hub_client, desk_client));
        desk.stop().await.unwrap();
        hub.stop().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streaming_download_survives_prune_and_releases_its_pin_at_shutdown() {
    use ritornel_client::contract::methods::TranscodePrune;
    use ritornel_client::contract::transcode::TranscodePruneParams;
    use ritornel_engine::transcode::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    bounded(async {
        let scratch = Scratch::new();
        let (hub, hub_client, desk, desk_client) = two(&scratch).await;
        join(&desk_client, &invite(&hub, &hub_client).await).await.unwrap();
        let link: String = rusqlite::Connection::open(scratch.data_dir("desk").join("server.db"))
            .unwrap().query_row("SELECT token FROM hub_link", [], |r| r.get(0)).unwrap();
        drop((hub_client, desk_client));
        desk.stop().await.unwrap();
        hub.stop().await.unwrap();

        // Publish a synthetic file larger than the socket buffers while the hub is offline.
        let root = scratch.data_dir("hub");
        let store = ritornel_engine::ServerStore::open(&root.join("server.db"), Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap();
        let cache = TranscodeCache::open(&store, &root.join("cache"), key(), TranscodeCache::DEFAULT_BUDGET).unwrap();
        let pin = Pin { id: key(), owner: PinOwner::SyncBatch };
        let entry = cache.derivative(&store, Derivative {
            asset:AudioAsset { scheme:1, hash:[81;32].into() },
            encoder: &FakeEncoder.probe().unwrap().unwrap(),
            profile:Profile { target:Target::Mp3 {mode:Mp3::V2}, dsp:Default::default() },
            gain_tags: Default::default()
        }, pin, |path,_| std::fs::File::create(path)?.set_len(64 << 20)).unwrap();
        cache.release(&store, pin.id).unwrap();
        store.close().unwrap();

        let hub = start(&scratch,"hub").await;
        let mut c = client(&scratch,"hub",&hub).await;
        let ep = hub.endpoint();
        let mut socket = tokio::net::TcpStream::connect(ep).await.unwrap();
        socket.write_all(format!("GET /v1/cache/derivative/{} HTTP/1.1\r\nHost: {ep}\r\nAuthorization: Bearer {link}\r\n\r\n",entry.key()).as_bytes()).await.unwrap();
        let mut download = BufReader::new(socket);
        let mut first = String::new();
        download.read_line(&mut first).await.unwrap();
        assert!(first.contains("200 OK"), "{first}");
        c.subscribe(&[Topic::Jobs]).await.unwrap();
        let job = c.call::<TranscodePrune>(&TranscodePruneParams {idempotency_key:key()}).await.unwrap().job;
        loop {
            if let Event::Jobs(event) = c.next_event().await.unwrap()
                && event.job.job_id == job.job_id && event.job.state.ended() {
                assert_eq!(event.job.state, ritornel_client::contract::JobState::Succeeded);
                break;
            }
        }
        assert!(entry.path().exists());
        drop(c);
        hub.stop().await.unwrap(); // The client still holds its end of the transfer.
        assert!(!root.join("server.db-wal").exists());
        let db = rusqlite::Connection::open(root.join("server.db")).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM transcode_pin", [], |r| r.get::<_,i64>(0)).unwrap(),0);
    }).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn players_are_listed_from_either_server_and_the_local_player_stays_available_offline() {
    use ritornel_client::contract::methods::PlayersListCall;
    bounded(async {
        let scratch = Scratch::new();
        let hub = start_playing_with(&scratch, "hub", 1, None).await;
        let mut hub_client = client(&scratch, "hub", &hub).await;
        hub_client.create_library(key()).await.unwrap();
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        let mut desk_client = client(&scratch, "desk", &desk).await;
        assert!(
            ritornel_client::descriptor(hub.endpoint())
                .await
                .unwrap()
                .has(capability::PLAYERS)
        );
        assert!(
            !ritornel_client::descriptor(desk.endpoint())
                .await
                .unwrap()
                .has(capability::PLAYERS)
        );
        hub_client.subscribe(&[Topic::Players]).await.unwrap();
        join(&desk_client, &invite(&hub, &hub_client).await)
            .await
            .unwrap();
        assert!(matches!(
            hub_client.next_event().await.unwrap(),
            Event::Players
        ));
        let list = hub_client
            .call::<PlayersListCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(list.players.len(), 2);
        for p in &list.players {
            assert!(p.reachable);
            assert_eq!(p.is_local, p.server_id == hub.server_id());
            assert_eq!(p.status, Some(PlaybackStatus::Stopped));
            assert!(p.output.is_none());
        }
        assert!(
            ritornel_client::descriptor(desk.endpoint())
                .await
                .unwrap()
                .has(capability::PLAYERS)
        );
        let from_desk = desk_client
            .call::<PlayersListCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(from_desk.players.len(), 2);
        for p in from_desk.players {
            let from_hub = list
                .players
                .iter()
                .find(|q| q.player_id == p.player_id)
                .unwrap();
            assert_eq!(p.is_local, p.server_id == desk.server_id());
            assert_eq!(p.since_ms, from_hub.since_ms);
        }
        desk_client.subscribe(&[Topic::Players]).await.unwrap();
        hub.rename("Hall speakers").unwrap();
        loop {
            assert!(matches!(
                desk_client.next_event().await.unwrap(),
                Event::Players
            ));
            let rows = desk_client
                .call::<PlayersListCall>(&NoParams {})
                .await
                .unwrap();
            if rows
                .players
                .iter()
                .any(|p| p.server_id == hub.server_id() && p.name == "Hall speakers")
            {
                break;
            }
        }
        desk.rename("Desk speakers").unwrap();
        loop {
            assert!(matches!(
                hub_client.next_event().await.unwrap(),
                Event::Players
            ));
            let rows = hub_client
                .call::<PlayersListCall>(&NoParams {})
                .await
                .unwrap();
            let row = rows
                .players
                .iter()
                .find(|p| p.server_id == desk.server_id())
                .unwrap();
            let before = list
                .players
                .iter()
                .find(|p| p.server_id == desk.server_id())
                .unwrap();
            assert!(row.reachable);
            assert_eq!(row.since_ms, before.since_ms);
            if row.name == "Desk speakers" {
                break;
            }
        }
        let port = hub.endpoint().port();
        hub.stop().await.unwrap();
        let offline = desk_client
            .call::<PlayersListCall>(&NoParams {})
            .await
            .unwrap();
        assert_eq!(offline.players.len(), 1);
        assert_eq!(offline.players[0].server_id, desk.server_id());
        assert!(offline.players[0].is_local && offline.players[0].reachable);
        desk.stop().await.unwrap();
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        assert!(
            ritornel_client::descriptor(desk.endpoint())
                .await
                .unwrap()
                .has(capability::PLAYERS)
        );
        let hub = restart_hub(&scratch, port).await;
        let mut hub_client = client(&scratch, "hub", &hub).await;
        hub_client.subscribe(&[Topic::Players]).await.unwrap();
        loop {
            let rows = hub_client
                .call::<PlayersListCall>(&NoParams {})
                .await
                .unwrap();
            let peer = rows
                .players
                .iter()
                .find(|p| p.player_id == offline.players[0].player_id)
                .expect("the hub remembers its player across restart");
            if peer.reachable {
                break;
            }
            assert!(matches!(
                hub_client.next_event().await.unwrap(),
                Event::Players
            ));
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hub_reports_what_plays_on_the_satellite_and_its_open_output() {
    use ritornel_client::contract::methods::PlayersListCall;
    bounded(async {
        let scratch = Scratch::new();
        let (_hub, mut hub_client, desk, desk_client) = linked_player_at(&scratch, 1).await;
        let Row::Song(song) = desk_client.list_open(&songs()).await.unwrap().rows[0].clone() else {
            panic!("the scanned track is missing");
        };
        let title = song.title.unwrap();
        hub_client.subscribe(&[Topic::Players]).await.unwrap();
        // The same track keeps playing while the hub receives its state.
        desk_client
            .call_raw(
                "playback.set",
                serde_json::json!({"idempotencyKey":key(),"repeat":"one"}),
            )
            .await
            .unwrap();
        desk_client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                player: None,
                idempotency_key: key(),
                source: PlaySource::Tracks {
                    ids: vec![song.appearance_id],
                },
                start: None,
                context: None,
                shuffle: None,
            })
            .await
            .unwrap();
        loop {
            assert!(matches!(
                hub_client.next_event().await.unwrap(),
                Event::Players
            ));
            let list = hub_client
                .call::<PlayersListCall>(&NoParams {})
                .await
                .unwrap();
            if let Some(player) = list.players.iter().find(|p| {
                p.server_id == desk.server_id() && p.status == Some(PlaybackStatus::Playing)
            }) {
                assert_eq!(player.title.as_ref(), Some(&title));
                assert_eq!(player.output.as_ref().unwrap().name, "null");
                break;
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_limited_controller_routes_to_a_linked_player_and_replays_its_receipt() {
    use ritornel_contract::{SessionKind, methods::PlayersListCall};
    use serde_json::json;

    bounded(async {
        let scratch = Scratch::new();
        let (hub, mut observer, desk, desk_client) = linked_player_at(&scratch, 1).await;
        let id = desk.server_id();
        observer.subscribe(&[Topic::Players]).await.unwrap();
        loop {
            let list = observer
                .call::<PlayersListCall>(&NoParams {})
                .await
                .unwrap();
            if list
                .players
                .iter()
                .any(|p| p.player_id == id && p.reachable)
            {
                break;
            }
            observer.next_event().await.unwrap();
        }
        let (tx, rx) = hub.connect_in_memory(SessionKind::Limited);
        let limited = Client::over_memory(tx, rx);
        limited
            .hello(&HelloParams {
                name: "controller".into(),
                kind: ClientKind::Cli,
                version: "1".into(),
                contract_version: CONTRACT_VERSION,
                presence: None,
            })
            .await
            .unwrap();

        let songs = limited.list_open(&songs()).await.unwrap();
        let Row::Song(song) = songs.rows[0].clone() else {
            panic!("no scanned track");
        };
        let source = PlaySource::Tracks {
            ids: vec![song.appearance_id],
        };
        let request = json!({"idempotencyKey":key(), "source":source, "place":{"type":"end"}});
        let mut routed = request.clone();
        routed["player"] = json!(id);
        let receipt = limited.call_raw("queue.add", routed.clone()).await.unwrap();
        assert_eq!(receipt["commandId"], request["idempotencyKey"]);
        assert_eq!(
            limited.call_raw("queue.add", routed).await.unwrap(),
            receipt
        );
        assert_eq!(
            desk_client.call_raw("queue.add", request).await.unwrap(),
            receipt
        );

        // A second satellite's controller goes up to the hub and down to the
        // player, which must read the track's title back from the hub.
        let other = start(&scratch, "other").await;
        let other_client = client(&scratch, "other", &other).await;
        join(&other_client, &invite(&hub, &observer).await)
            .await
            .unwrap();
        let window = other_client
            .call_raw("queue.window", json!({"player":id}))
            .await
            .unwrap();
        assert_eq!(window["total"], 1);
        assert_eq!(window["entries"][0]["track"]["title"], song.title.unwrap());
        assert_eq!(
            window,
            desk_client
                .call_raw("queue.window", json!({}))
                .await
                .unwrap()
        );
        let state = limited
            .call_raw("playback.get", json!({"player":id}))
            .await
            .unwrap();
        assert_eq!(state["queueLength"], 1);

        for target in [hub.server_id(), other.server_id(), key()] {
            let Err(ritornel_client::Error::Call(error)) = limited
                .call_raw("playback.get", json!({"player":target}))
                .await
            else {
                panic!("expected a no_player error");
            };
            assert_eq!(error.kind(), ErrorCode::Unsupported);
            assert_eq!(error.data.details["reason"], "no_player");
        }
        desk.stop().await.unwrap();
        let away = loop {
            let list = observer
                .call::<PlayersListCall>(&NoParams {})
                .await
                .unwrap();
            if let Some(p) = list
                .players
                .into_iter()
                .find(|p| p.player_id == id && !p.reachable)
            {
                break p;
            }
            observer.next_event().await.unwrap();
        };
        let Err(ritornel_client::Error::Call(error)) = other_client
            .call_raw("playback.get", json!({"player":id}))
            .await
        else {
            panic!("expected a player_unreachable error");
        };
        assert_eq!(error.kind(), ErrorCode::Unavailable);
        assert_eq!(error.data.details["reason"], "player_unreachable");
        assert_eq!(error.data.details["name"], away.name);
        assert_eq!(error.data.details["sinceMs"], away.since_ms);
    })
    .await;
}

/// Add reachable song rows to the scratch catalog without generating thousands of audio files.
fn add_view_rows(scratch: &Scratch, count: u64) {
    let mut db = rusqlite::Connection::open(scratch.data_dir("hub").join("library.db")).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute(
        "CREATE TEMP TABLE added AS
         WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ?1)
         SELECT n + max((SELECT max(id) FROM source_item), (SELECT max(id) FROM track_appearance)) AS id FROM seq",
        [i64::try_from(count).unwrap()],
    ).unwrap();
    tx.execute_batch(
        "INSERT INTO track_appearance(id, public_id, recording_id, canonical_id, display_title, rev)
           SELECT id, randomblob(16), (SELECT min(id) FROM recording), id, 'A fixture ' || id, 1 FROM added;
         INSERT INTO source_item(id, public_id, source_instance_id, item_kind, provider_key, first_seen_at_ms, rev)
           SELECT id, randomblob(16), (SELECT min(source_instance_id) FROM source_item), 'file', 'row-' || id, 1, 1 FROM added;
         INSERT INTO assignment(source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
           SELECT id, (SELECT min(id) FROM recording), id, (SELECT min(id) FROM identity_decision), 1, 1 FROM added;",
    ).unwrap();
    tx.commit().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_view_over_twenty_thousand_rows_is_refused_before_loading_the_player() {
    use serde_json::json;

    bounded(async {
        let scratch = Scratch::new();
        let (_hub, mut controller, desk, local) = linked_player_at(&scratch, 1).await;
        wait_for_player(&mut controller, desk.server_id()).await;
        let original = controller.list_open(&songs()).await.unwrap();
        add_view_rows(&scratch, 20_001 - original.total);
        let opened = controller.list_open(&songs()).await.unwrap();
        assert_eq!(opened.total, 20_001);
        let error = controller
            .call_raw(
                "queue.add",
                json!({
                    "player":desk.server_id(), "idempotencyKey":key(),
                    "source":{"type":"view","view":opened.view}, "place":{"type":"end"}
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(code(&error), Some(ErrorCode::Invalid));
        assert!(error.to_string().contains("20000"));
        assert_eq!(
            local
                .call::<PlaybackGet>(&PlaybackGetParams::default())
                .await
                .unwrap()
                .queue_length,
            0
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn routed_views_keep_their_order_context_and_receipt_after_close() {
    use serde_json::json;

    bounded(async {
        let scratch = Scratch::new();
        let (hub, mut controller, desk, local) = linked_player_at(&scratch, 1).await;
        let target = desk.server_id();
        wait_for_player(&mut controller, target).await;
        add_view_rows(&scratch, 2);
        let opened = controller.list_open(&songs()).await.unwrap();
        let mut tracks = song_ids(&opened.rows);
        let fixtures: Vec<_> = opened
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let Row::Song(song) = row else { return None };
                song.title
                    .as_deref()?
                    .starts_with("A fixture")
                    .then_some((index, song.appearance_id))
            })
            .collect();
        let [(gone, first), (_, second)] = fixtures[..] else {
            panic!("expected two added rows")
        };
        let start = tracks.len() - 1;
        assert!(gone < start);
        let retire = |id: ritornel_contract::Id| {
            let db =
                rusqlite::Connection::open(scratch.data_dir("hub").join("library.db")).unwrap();
            // The fixture uses the appearance's row key for its source item.
            let changed = db
                .execute(
                    "UPDATE source_item SET retired_at_ms = 1
                 WHERE id = (SELECT id FROM track_appearance WHERE public_id = ?1)",
                    [id.as_bytes()],
                )
                .unwrap();
            assert_eq!(changed, 1);
        };
        retire(first);
        tracks.remove(gone);
        let source = json!({"type":"view","view":opened.view});
        let refused = controller
            .call_raw(
                "playback.play",
                json!({
                    "player":target, "idempotencyKey":key(), "source":source, "start":gone
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(code(&refused), Some(ErrorCode::NotFound));
        let play = json!({"player":target,"idempotencyKey":key(),"source":source,"start":start});
        let add = json!({
            "player": target, "idempotencyKey": key(), "source": source,
            "place": {"type":"end"}, "context": {"kind":"artist","ref":key()}
        });
        let played = controller
            .call_raw("playback.play", play.clone())
            .await
            .unwrap();
        let window = local
            .call::<QueueWindow>(&QueueWindowParams {
                player: None,
                offset: 0,
                limit: None,
                order: None,
            })
            .await
            .unwrap();
        assert_eq!(played["entryId"], json!(window.entries[start - 1].entry_id));
        assert_eq!(
            window
                .entries
                .iter()
                .map(|e| e.track.appearance_id)
                .collect::<Vec<_>>(),
            tracks
        );
        let added = controller.call_raw("queue.add", add.clone()).await.unwrap();
        // Re-resolving the still-open view would now give a different queue.
        retire(second);
        assert_eq!(
            controller.call_raw("queue.add", add.clone()).await.unwrap(),
            added
        );
        assert!(controller.list_close(&opened.view).await.unwrap().closed);
        assert_eq!(
            controller
                .call_raw("playback.play", play.clone())
                .await
                .unwrap(),
            played
        );
        assert_eq!(
            controller.call_raw("queue.add", add.clone()).await.unwrap(),
            added
        );
        let contexts = {
            let db =
                rusqlite::Connection::open(scratch.data_dir("desk").join("server.db")).unwrap();
            db.prepare("SELECT context_kind, context_ref FROM queue_entry ORDER BY position")
                .unwrap()
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<[u8; 16]>>(1)?))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(contexts.len(), tracks.len() * 2);
        assert!(
            contexts[..tracks.len()]
                .iter()
                .all(|(kind, id)| kind == "query" && id.is_none())
        );
        let reference: ritornel_contract::Id =
            serde_json::from_value(add["context"]["ref"].clone()).unwrap();
        assert!(
            contexts[tracks.len()..]
                .iter()
                .all(|(kind, id)| kind == "artist" && id.as_ref() == Some(reference.as_bytes()))
        );
        let stranger = client(&scratch, "hub", &hub).await;
        assert_eq!(
            code(
                &stranger
                    .call_raw("playback.play", play.clone())
                    .await
                    .unwrap_err()
            ),
            Some(ErrorCode::ViewExpired)
        );
        let mut changed = play;
        changed["start"] = json!(0);
        assert_eq!(
            code(
                &controller
                    .call_raw("playback.play", changed)
                    .await
                    .unwrap_err()
            ),
            Some(ErrorCode::IdempotencyMismatch)
        );

        // A client cannot certify unknown tracks, even when asking through the hub.
        for method in ["queue.add", "playback.play"] {
            let invalid = json!({
                "source":{"type":"tracks","ids":[key()]}, "idempotencyKey":key(),
                "tracksKnown":true, "place":{"type":"end"}
            });
            assert_eq!(
                code(&local.call_raw(method, invalid.clone()).await.unwrap_err()),
                Some(ErrorCode::NotFound)
            );
            let mut routed = invalid;
            routed["player"] = json!(target);
            assert_eq!(
                code(&controller.call_raw(method, routed).await.unwrap_err()),
                Some(ErrorCode::NotFound)
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_satellite_controller_can_play_its_song_view_on_another_satellite() {
    use serde_json::json;

    bounded(async {
        let scratch = Scratch::new();
        let (hub, mut observer, desk, local) = linked_player_at(&scratch, 1).await;
        wait_for_player(&mut observer, desk.server_id()).await;
        let other = start(&scratch, "other").await;
        let controller = client(&scratch, "other", &other).await;
        join(&controller, &invite(&hub, &observer).await)
            .await
            .unwrap();
        let play = |view| {
            json!({
                "player": desk.server_id(), "idempotencyKey": key(),
                "source": {"type":"view", "view":view}, "shuffle":true
            })
        };
        let albums = controller
            .list_open(&ListOpenParams {
                kind: ListKind::Albums,
                ..songs()
            })
            .await
            .unwrap();
        let error = controller
            .call_raw("playback.play", play(albums.view))
            .await
            .unwrap_err();
        assert_eq!(code(&error), Some(ErrorCode::Invalid));
        let view = controller.list_open(&songs()).await.unwrap();
        let request = play(view.view.clone());
        let played = controller
            .call_raw("playback.play", request.clone())
            .await
            .unwrap();
        let state = local
            .call::<PlaybackGet>(&PlaybackGetParams::default())
            .await
            .unwrap();
        assert!(state.shuffle);
        assert_eq!(state.queue_length, view.total);
        assert!(controller.list_close(&view.view).await.unwrap().closed);
        assert_eq!(
            controller.call_raw("playback.play", request).await.unwrap(),
            played
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_player_loads_hub_checked_tracks_without_validation_round_trips() {
    use serde_json::json;

    bounded(async {
        let scratch = Scratch::new();
        let hub = start(&scratch, "hub").await;
        let mut controller = client(&scratch, "hub", &hub).await;
        controller.create_library(key()).await.unwrap();
        scan(&scratch, &mut controller).await;
        let relay = Relay::to(hub.endpoint()).await;
        let link = controller
            .call::<LinkInvite>(&LinkInviteParams {
                routes: vec![relay.address.to_string()],
            })
            .await
            .unwrap()
            .link;
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        let local = client(&scratch, "desk", &desk).await;
        join(&local, &link).await.unwrap();
        wait_for_player(&mut controller, desk.server_id()).await;

        let current = song_ids(&controller.list_open(&songs()).await.unwrap().rows)[0];
        add_view_rows(&scratch, 1);
        let probe = song_ids(&controller.list_open(&songs()).await.unwrap().rows)
            .into_iter().find(|id| *id != current).unwrap();
        let request = |ids| json!({"idempotencyKey":key(),"source":{"type":"tracks","ids":ids},"place":{"type":"end"}});
        // Populate the stopped player's current entry (which reads its title once).
        local
            .call_raw("queue.add", request(vec![current]))
            .await
            .unwrap();
        // Count only summaries of a song other than the current entry, so a
        // background read of the current song cannot affect the measurement.
        let reply = format!(r#""appearanceId":"{probe}""#);
        let count = || {
            let heard = relay.heard.lock().unwrap();

            heard.windows(reply.len()).filter(|w| *w == reply.as_bytes()).count()
        };
        let before = count();
        let ids = vec![probe; 1001];
        let mut routed = request(ids.clone());
        routed["player"] = json!(desk.server_id());
        controller.call_raw("queue.add", routed).await.unwrap();
        assert_eq!(count(), before);
        local.call_raw("queue.add", request(ids)).await.unwrap();
        assert_eq!(
            count(),
            before + 1001,
            "local validation reads a summary for each submitted ID"
        );
        assert_eq!(
            local
                .call::<PlaybackGet>(&PlaybackGetParams::default())
                .await
                .unwrap()
                .queue_length,
            2003
        );
    }).await;
}

async fn wait_for_player(observer: &mut Client, id: ritornel_contract::Id) {
    observer.subscribe(&[Topic::Players]).await.unwrap();
    loop {
        let rows = observer
            .call::<ritornel_contract::methods::PlayersListCall>(&NoParams {})
            .await
            .unwrap();
        if rows
            .players
            .iter()
            .any(|p| p.player_id == id && p.reachable)
        {
            break;
        }
        observer.next_event().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn player_subscriptions_follow_their_target_through_the_hub_and_a_restart() {
    use ritornel_contract::Id;
    use serde_json::json;

    let set = async |c: &Client, volume: f64| {
        c.call_raw(
            "playback.set",
            json!({"idempotencyKey":key(),"volume":volume}),
        )
        .await
        .unwrap();
    };
    let volume = async |c: &mut Client, player: Option<Id>, volume: f64| {
        loop {
            if let Event::Playback(e) = c.next_event().await.unwrap() {
                assert_eq!(e.player, player);
                if (e.state.volume - volume).abs() < 0.0001 {
                    break;
                }
            }
        }
    };
    bounded(async {
        let scratch = Scratch::new();
        let hub = start_playing_with(&scratch, "hub", 1, None).await;
        let h = client(&scratch, "hub", &hub).await;
        h.create_library(key()).await.unwrap();
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        let mut d = client(&scratch, "desk", &desk).await;
        join(&d, &invite(&hub, &h).await).await.unwrap();
        let other = start(&scratch, "other").await;
        let mut o = client(&scratch, "other", &other).await;
        join(&o, &invite(&hub, &h).await).await.unwrap();
        let id = desk.server_id();
        let hub_id = hub.server_id();
        let target = json!({"player":id,"topics":["playback"]});
        let hub_target = json!({"player":hub_id,"topics":["playback"]});
        // Wait for the presence hello before changing the remote player.
        let mut observer = client(&scratch, "hub", &hub).await;
        wait_for_player(&mut observer, id).await;
        assert_eq!(
            o.call_raw("events.subscribe", target.clone())
                .await
                .unwrap(),
            target
        );
        o.subscribe(&[Topic::Servers]).await.unwrap();
        d.subscribe(&[Topic::Playback]).await.unwrap();
        assert_eq!(
            d.call_raw("events.subscribe", hub_target.clone())
                .await
                .unwrap(),
            hub_target
        );
        set(&d, 0.31).await;
        volume(&mut d, None, 0.31).await;
        volume(&mut o, Some(id), 0.31).await;
        set(&h, 0.42).await;
        volume(&mut d, Some(hub_id), 0.42).await;
        assert_eq!(
            d.call_raw("events.unsubscribe", hub_target).await.unwrap(),
            json!({"player":hub_id,"topics":[]})
        );
        set(&h, 0.69).await;
        set(&d, 0.53).await;
        volume(&mut d, None, 0.53).await;
        volume(&mut o, Some(id), 0.53).await;

        let port = hub.endpoint().port();
        hub.stop().await.unwrap();
        hub_answers(&mut o, false).await;
        let restarted = start_on(&scratch, "hub", Listen::on_port(port)).await;
        hub_answers(&mut o, true).await;
        let mut observer = client(&scratch, "hub", &restarted).await;
        wait_for_player(&mut observer, id).await;
        set(&d, 0.64).await;
        volume(&mut o, Some(id), 0.64).await;
        // Keep the controller and its hub connection. A restarted player that
        // stays stopped must still refresh both playback and queue watchers.
        o.call_raw("events.subscribe", json!({"player":id,"topics":["queue"]}))
            .await
            .unwrap();
        desk.stop().await.unwrap();
        let _desk = start_playing_with(&scratch, "desk", 1, None).await;
        let mut seen_state = false;
        let mut seen_queue = false;
        while !(seen_state && seen_queue) {
            match o.next_event().await.unwrap() {
                Event::Playback(e) if e.player == Some(id) => {
                    assert_eq!(e.state.status, PlaybackStatus::Stopped);
                    seen_state = true;
                }
                Event::Queue(e) if e.player == Some(id) => seen_queue = true,
                _ => {}
            }
        }
        o.call_raw(
            "events.unsubscribe",
            json!({"player":id,"topics":["queue"]}),
        )
        .await
        .unwrap();
        assert_eq!(
            o.call_raw("events.unsubscribe", target).await.unwrap(),
            json!({"player":id,"topics":[]})
        );
    })
    .await;
}

/// A hub with a real player and a track long enough to stay current throughout the move.
async fn move_hub(scratch: &Scratch) -> (Server, Client, ritornel_contract::Id) {
    let hub = start_playing_with(scratch, "hub", 1, None).await;
    let mut c = client(scratch, "hub", &hub).await;
    c.create_library(key()).await.unwrap();
    super::launch::long_track(&scratch.0.0.join("music"));
    scan(scratch, &mut c).await;
    let tone = c
        .list_open(&songs())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find_map(|r| match r {
            Row::Song(s) if s.duration_ms == Some(40_000) => Some(s.appearance_id),
            _ => None,
        })
        .unwrap();
    (hub, c, tone)
}

async fn move_track(c: &Client, id: ritornel_contract::Id) -> serde_json::Value {
    use serde_json::json;
    c.call_raw("playback.play", json!({"idempotencyKey":key(),"source":{"type":"tracks","ids":[id,id,id]},"shuffle":true,"context":{"kind":"query"}})).await.unwrap();
    loop {
        let s = c.call_raw("playback.get", json!({})).await.unwrap();
        if s["status"] == "playing" && s["positionMs"].as_u64().unwrap() > 0 {
            return s;
        }
    }
}

async fn wait_empty(c: &Client) {
    while c
        .call::<PlaybackGet>(&PlaybackGetParams::default())
        .await
        .unwrap()
        .queue_length
        != 0
    {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_music_between_three_players_keeps_queue_order_position_and_one_receipt() {
    use serde_json::json;
    bounded(async {
        let scratch = Scratch::new();
        let (hub, mut h, tone) = move_hub(&scratch).await;
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        let d = client(&scratch, "desk", &desk).await;
        join(&d, &invite(&hub, &h).await).await.unwrap();
        wait_for_player(&mut h, desk.server_id()).await;
        let other = start_playing_with(&scratch, "other", 1, None).await;
        let o = client(&scratch, "other", &other).await;
        join(&o, &invite(&hub, &h).await).await.unwrap();
        wait_for_player(&mut h, other.server_id()).await;
        for c in [&h, &d] {
            let refused = c.call_raw("link.handoff", json!({})).await.unwrap_err();
            assert_eq!(code(&refused), Some(ErrorCode::Forbidden));
        }

        move_track(&d, tone).await;
        let original = move_track(&h, tone).await;
        h.call_raw(
            "playback.set",
            json!({"idempotencyKey":key(),"repeat":"one"}),
        )
        .await
        .unwrap();
        let queue = |order| json!({"order":order});
        let ordered = h.call_raw("queue.window", queue("play")).await.unwrap()["entries"].clone();
        let move_to =
            |from, to, entry| json!({"idempotencyKey":key(),"from":from,"to":to,"entryId":entry});
        let stale = move_to(hub.server_id(), desk.server_id(), json!(key()));
        assert_eq!(
            code(&h.call_raw("playback.move", stale).await.unwrap_err()),
            Some(ErrorCode::Invalid)
        );
        let same = move_to(
            hub.server_id(),
            hub.server_id(),
            original["entryId"].clone(),
        );
        assert_eq!(
            code(&h.call_raw("playback.move", same).await.unwrap_err()),
            Some(ErrorCode::Invalid)
        );
        let first = move_to(
            hub.server_id(),
            desk.server_id(),
            original["entryId"].clone(),
        );
        let receipt = o.call_raw("playback.move", first.clone()).await.unwrap();
        assert_eq!(receipt["status"], "done");
        assert_eq!(h.call_raw("playback.move", first).await.unwrap(), receipt);
        wait_empty(&h).await;
        // A transferred position alone does not prove the destination has
        // rendered a frame. Wait for the continued listen to open there.
        let desk_db =
            rusqlite::Connection::open(scratch.data_dir("desk").join("server.db")).unwrap();
        while !desk_db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM listen_staging WHERE continues_listen_id IS NOT NULL
             AND ended_at_utc_ms IS NULL)",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
        {
            tokio::task::yield_now().await;
        }
        d.call_raw("playback.pause", json!({"idempotencyKey":key()}))
            .await
            .unwrap();
        // Seeking while paused gives the source a known position after the
        // last output callback has given up the playing span.
        d.call_raw(
            "playback.seek",
            json!({"idempotencyKey": key(), "entryId": original["entryId"], "positionMs": 12_000}),
        )
        .await
        .unwrap();
        let paused = d.call_raw("playback.get", json!({})).await.unwrap();
        assert_eq!(paused["positionMs"], 12_000);
        let second = move_to(
            desk.server_id(),
            other.server_id(),
            paused["entryId"].clone(),
        );
        d.call_raw("playback.move", second).await.unwrap();
        wait_empty(&d).await;
        let moved = o.call_raw("playback.get", json!({})).await.unwrap();
        for field in [
            "status",
            "positionMs",
            "entryId",
            "repeat",
            "shuffle",
            "queueLength",
        ] {
            assert_eq!(moved[field], paused[field], "{field}");
        }
        assert_eq!(moved["status"], "paused");
        assert_eq!(
            o.call_raw("queue.window", queue("play")).await.unwrap()["entries"],
            ordered
        );
        let db = rusqlite::Connection::open(scratch.data_dir("other").join("server.db")).unwrap();
        let query: i64 = db
            .query_row(
                "SELECT count(*) FROM queue_entry WHERE context_kind='query'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(query, 3);

        // Both the hub and Desk have released their part of the moved
        // listen. Desk also closed the listen it was playing beforehand.
        let history =
            rusqlite::Connection::open(scratch.data_dir("hub").join("library.db")).unwrap();
        while history
            .query_row("SELECT count(*) FROM observation_listen", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
            < 3
        {
            tokio::task::yield_now().await;
        }
        let joined: i64 = history
            .query_row(
                "SELECT count(*) FROM (SELECT p.play_event_id FROM play_event_observation p
             JOIN history_observation o ON o.id=p.observation_id GROUP BY p.play_event_id
             HAVING count(DISTINCT o.endpoint_id)=2 AND count(*)=2)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(joined, 1);
        let replaced: i64 = history
            .query_row(
                "SELECT count(*) FROM observation_listen WHERE end_reason='replaced'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(replaced, 1);
    })
    .await;
}

async fn scripted_peer(
    scratch: &Scratch,
    hub: &Server,
) -> (
    Client,
    tokio::sync::mpsc::Receiver<ritornel_contract::Request>,
) {
    use serde_json::json;
    let token: String = rusqlite::Connection::open(scratch.data_dir("desk").join("server.db"))
        .unwrap()
        .query_row("SELECT token FROM hub_link", [], |r| r.get(0))
        .unwrap();
    let mut c = Client::connect_peer(hub.endpoint(), &Credential::new(&token))
        .await
        .unwrap();
    let requests = c.take_requests().unwrap();
    c.call_raw(
        "server.hello",
        json!({"kind":"server","name":"Desk","version":"1","contractVersion":CONTRACT_VERSION,
        "presence":{"hasPlayer":true,"sendsNowPlaying":false,"player":{"status":"paused"}}}),
    )
    .await
    .unwrap();
    (c, requests)
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_load_preserves_the_source_and_an_unanswered_release_retries_on_reconnect() {
    use ritornel_contract::{Request, Response, RpcError};
    use serde_json::json;
    bounded(async {
        let scratch = Scratch::new();
        let (hub, mut h, tone) = move_hub(&scratch).await;
        let desk = start(&scratch, "desk").await;
        let id = desk.server_id();
        let d = client(&scratch, "desk", &desk).await;
        join(&d, &invite(&hub, &h).await).await.unwrap();
        drop(d);
        desk.stop().await.unwrap();
        let (peer, mut requests) = scripted_peer(&scratch, &hub).await;
        wait_for_player(&mut h, id).await;
        let entry = move_track(&h, tone).await["entryId"].clone();
        let outbound =
            json!({"idempotencyKey":key(),"entryId":entry,"from":hub.server_id(),"to":id});
        let respond = |c: &Client, r: Request, outcome| {
            c.respond(Response {
                id: Some(r.id),
                outcome,
            })
            .unwrap()
        };
        let (result, snapshot) = tokio::join!(h.call_raw("playback.move", outbound), async {
            let r = requests.recv().await.unwrap();
            assert_eq!(r.method, "link.handoff");
            let snapshot = r.params.as_ref().unwrap()["snapshot"].clone();
            respond(
                &peer,
                r,
                Err(RpcError::new(ErrorCode::Invalid, "cannot accept queue")),
            );
            snapshot
        });
        assert_eq!(code(&result.unwrap_err()), Some(ErrorCode::Invalid));
        let state = h.call_raw("playback.get", json!({})).await.unwrap();
        assert_eq!(state["status"], "paused");
        assert_eq!(state["queueLength"], 3);
        let db = rusqlite::Connection::open(scratch.data_dir("hub").join("server.db")).unwrap();
        let open: i64 = db
            .query_row(
                "SELECT count(*) FROM listen_staging WHERE ended_at_utc_ms IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 1);

        let inbound =
            json!({"idempotencyKey":key(),"entryId":entry,"from":id,"to":hub.server_id()});
        let (receipt, ()) = tokio::join!(h.call_raw("playback.move", inbound.clone()), async {
            loop {
                let r = requests.recv().await.unwrap();
                if r.params.as_ref().unwrap()["step"] == "take" {
                    respond(&peer, r, Ok(json!({"step":"taken","snapshot":snapshot})));
                    break;
                }
                respond(&peer, r, Ok(json!({"step":"notLoaded"})));
            }
        });
        let receipt = receipt.unwrap();
        loop {
            let r = requests.recv().await.unwrap();
            if r.params.as_ref().unwrap()["step"] == "release" {
                break;
            }
            respond(&peer, r, Ok(json!({"step":"notLoaded"})));
        }
        drop(peer);
        drop(db);
        drop(h);
        hub.stop().await.unwrap();
        let hub = start_playing_with(&scratch, "hub", 1, None).await;
        let h = client(&scratch, "hub", &hub).await;
        let (again, mut requests) = scripted_peer(&scratch, &hub).await;
        loop {
            let r = requests.recv().await.unwrap();
            if r.params.as_ref().unwrap()["step"] == "release" {
                assert_eq!(r.params.as_ref().unwrap()["movement"], inbound);
                respond(&again, r, Ok(json!({"step":"released"})));
                break;
            }
            respond(&again, r, Ok(json!({"step":"notLoaded"})));
        }
        assert_eq!(h.call_raw("playback.move", inbound).await.unwrap(), receipt);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_load_answer_is_confirmed_before_the_hub_releases_the_source() {
    recover_lost_load(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_player_retires_the_load_attempt_and_leaves_the_source_intact() {
    recover_lost_load(false).await;
}

async fn recover_lost_load(destination_kept_queue: bool) {
    use ritornel_contract::Response;
    use serde_json::json;
    bounded(async {
        let scratch = Scratch::new();
        let (hub, mut controller, tone) = move_hub(&scratch).await;
        let desk = start(&scratch, "desk").await;
        let id = desk.server_id();
        let c = client(&scratch, "desk", &desk).await;
        join(&c, &invite(&hub, &controller).await).await.unwrap();
        drop(c);
        desk.stop().await.unwrap();
        let (peer, mut requests) = scripted_peer(&scratch, &hub).await;
        wait_for_player(&mut controller, id).await;
        let state = move_track(&controller, tone).await;
        let movement = json!({
            "idempotencyKey": key(), "entryId": state["entryId"], "from": hub.server_id(), "to": id
        });
        let (lost, attempt) = tokio::join!(
            controller.call_raw("playback.move", movement.clone()),
            async {
                let load = requests.recv().await.unwrap().params.unwrap();
                assert_eq!(load["step"], "load");
                assert!(load["snapshot"].is_object());
                drop(peer);
                load["attempt"].clone()
            }
        );
        assert_eq!(code(&lost.unwrap_err()), Some(ErrorCode::Unavailable));
        let paused = controller
            .call::<PlaybackGet>(&PlaybackGetParams::default())
            .await
            .unwrap();
        assert_eq!(paused.queue_length, 3);
        let (again, mut requests) = scripted_peer(&scratch, &hub).await;
        let probe = requests.recv().await.unwrap();
        let params = probe.params.unwrap();
        assert_eq!(params["attempt"], attempt);
        assert!(params["snapshot"].is_null());
        let outcome = if destination_kept_queue {
            Ok(json!({"step": "loaded"}))
        } else {
            Err(
                ritornel_contract::RpcError::new(ErrorCode::Unsupported, "the player was removed")
                    .with_detail("reason", "no_player"),
            )
        };
        let answer = Response {
            id: Some(probe.id),
            outcome,
        };
        if destination_kept_queue {
            let (retry, ()) = tokio::join!(
                controller.call_raw("playback.move", movement.clone()),
                async {
                    tokio::task::yield_now().await;
                    again.respond(answer).unwrap();
                }
            );
            assert_eq!(retry.unwrap()["status"], "done");
        } else {
            again.respond(answer).unwrap();
            let db = rusqlite::Connection::open(scratch.data_dir("hub").join("server.db")).unwrap();
            while db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM server_setting WHERE key GLOB 'move.*')",
                    [],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap()
            {
                tokio::task::yield_now().await;
            }
            let source = controller
                .call::<PlaybackGet>(&PlaybackGetParams::default())
                .await
                .unwrap();
            assert_eq!(source.queue_length, paused.queue_length);
            assert_eq!(source.status, paused.status);
            return;
        }
        wait_empty(&controller).await;
        assert_eq!(
            controller
                .call_raw("playback.move", movement)
                .await
                .unwrap()["status"],
            "done"
        );
    })
    .await;
}

#[cfg(feature = "lastfm")]
#[path = "../../../../fixtures/support/tone.rs"]
mod tone;

#[cfg(feature = "lastfm")]
#[tokio::test(flavor = "multi_thread")]
async fn a_move_to_a_satellite_and_repeat_one_each_show_a_new_now_playing() {
    use ritornel_lastfm::fake::{API_KEY, API_SECRET, FakeLastfm};
    use serde_json::json;

    async fn shown(fake: &FakeLastfm, count: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while fake.now_playing().len() < count {
            assert!(
                std::time::Instant::now() < deadline,
                "waiting for update {count}: {:?}",
                fake.now_playing()
            );
            // sleep: poll only until the loopback service has received the settled start.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(fake.now_playing().len(), count);
    }

    bounded(async {
        let fake = FakeLastfm::start("someone", vec![]);
        let scratch = Scratch::new();
        let run = Run {
            playback: PlaybackRun {
                output: Some(Output::Null { speed: 1 }),
                ..PlaybackRun::default()
            },
            lastfm: ritornel_server::LastfmRun {
                credentials: Some(ritornel_lastfm::ApiCredentials::new(API_KEY, API_SECRET)),
                base_url: Some(fake.base_url()),
                ..Default::default()
            },
            ..Run::default()
        };
        let hub = start_with_run(&scratch, "hub", run).await;
        let mut h = client(&scratch, "hub", &hub).await;
        h.create_library(key()).await.unwrap();
        let folder = scratch.data_dir("music");
        std::fs::create_dir_all(&folder).unwrap();
        tone::write_aiff(&folder.join("01.aiff"), "Paper Kites", "1", 440.0, 40);
        tone::write_aiff(&folder.join("02.aiff"), "River Stones", "2", 660.0, 40);
        scan_folder(&mut h, &folder).await;
        let tracks = h.list_open(&songs()).await.unwrap().rows;
        let appearance = |title| {
            tracks
                .iter()
                .find_map(|row| match row {
                    Row::Song(song) if song.title.as_deref() == Some(title) => {
                        Some(song.appearance_id)
                    }
                    _ => None,
                })
                .unwrap()
        };
        fake.approve_after(0);
        let login = h.call_raw("lastfm.signInStart", json!({})).await.unwrap();
        let finish = json!({"signIn": login["signIn"], "idempotencyKey": key()});
        h.call_raw("lastfm.signInFinish", finish).await.unwrap();
        h.call_raw(
            "lastfm.setScrobbling",
            json!({"on": true, "idempotencyKey": key()}),
        )
        .await
        .unwrap();
        let desk = start_playing_with(&scratch, "desk", 1, None).await;
        let d = client(&scratch, "desk", &desk).await;
        join(&d, &invite(&hub, &h).await).await.unwrap();
        wait_for_player(&mut h, desk.server_id()).await;

        let state = move_track(&h, appearance("Paper Kites")).await;
        h.call_raw(
            "playback.set",
            json!({"idempotencyKey": key(), "repeat": "one"}),
        )
        .await
        .unwrap();
        shown(&fake, 1).await;
        let movement = json!({
            "idempotencyKey": key(), "from": hub.server_id(), "to": desk.server_id(),
            "entryId": state["entryId"]
        });
        h.call_raw("playback.move", movement).await.unwrap();
        shown(&fake, 2).await;
        let near_end =
            json!({"idempotencyKey": key(), "entryId": state["entryId"], "positionMs": 39_950});
        d.call_raw("playback.seek", near_end).await.unwrap();
        shown(&fake, 3).await;
        // Force the playing satellite to reconnect. Its new presence takes
        // over this socket and reports a snapshot of the track already heard.
        let (mut replacement, _requests) = scripted_peer(&scratch, &hub).await;
        assert!(replacement.next_event().await.is_none());
        // A later settled start on the hub gives the sender a chance to deliver
        // any extra report from the move or repeat before the final assertion.
        move_track(&h, appearance("River Stones")).await;
        shown(&fake, 4).await;
        let updates = fake.now_playing();
        assert_eq!(
            updates
                .iter()
                .map(|p| p["track"].as_str())
                .collect::<Vec<_>>(),
            ["Paper Kites", "Paper Kites", "Paper Kites", "River Stones"]
        );
        for fields in updates {
            assert_eq!(fields["artist"], "Invented Artist");
            assert_eq!(fields["album"], "Invented Album");
            assert_eq!(fields["duration"], "40");
        }
        assert!(fake.scrobbled().is_empty());
    })
    .await;
}
