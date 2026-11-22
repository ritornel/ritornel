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
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ritornel_client::contract::methods::{LinkInvite, LinkJoin, LinkStatusCall};
use ritornel_client::contract::{
    CONTRACT_VERSION, ClientKind, ErrorCode, HelloParams, InviteLink, LinkInviteParams,
    LinkJoinParams, LinkRequest, LinkStatus, NoParams, ServerRole, capability, hex, route,
};
use ritornel_client::{Client, Credential};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{HeldDir, Listen, Server};

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
    Server::start(
        HeldDir::take(scratch.data_dir(name)).unwrap(),
        Listen::on_port(0),
        Arc::new(SystemClock),
        Arc::new(OsEntropy),
    )
    .await
    .unwrap()
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

        // The secret is spent by the satellite that uses it first.
        let parsed = InviteLink::parse(&link).unwrap();
        let used = |server_id| LinkRequest {
            secret: hex::encode(&parsed.secret),
            server_id,
            name: "thief".to_owned(),
            contract_version: CONTRACT_VERSION,
        };
        let first =
            ritornel_client::post(hub.endpoint(), route::LINK, None, &used(other.server_id()))
                .await
                .unwrap();
        assert_eq!(first.status, 200);
        let again =
            ritornel_client::post(hub.endpoint(), route::LINK, None, &used(other.server_id()))
                .await
                .unwrap();
        assert_eq!(again.status, 403);
        let refused = join(&desk_client, &link).await.unwrap_err();
        assert!(
            refused.to_string().contains("refused the link"),
            "{refused}"
        );
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
            secret: hex::encode(&parsed.secret),
            server_id: desk.server_id(),
            name: "desk".to_owned(),
            contract_version: CONTRACT_VERSION + 1,
        };
        let answer = ritornel_client::post(hub.endpoint(), route::LINK, None, &asked)
            .await
            .unwrap();
        assert_eq!(answer.status, 409);
        assert!(String::from_utf8_lossy(&answer.body).contains("contract version"));
        // The invite is not spent by a refusal.
        let asked = LinkRequest {
            contract_version: CONTRACT_VERSION,
            ..asked
        };
        let answer = ritornel_client::post(hub.endpoint(), route::LINK, None, &asked)
            .await
            .unwrap();
        assert_eq!(answer.status, 200);
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
        // And it is given no ticket.
        assert!(matches!(
            ritornel_client::ticket(hub.endpoint(), &credential).await,
            Err(ritornel_client::Error::Refused { status: 403 })
        ));
    })
    .await;
}
