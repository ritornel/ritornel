//! The player's calls and events through `ritornel-client`: a real server
//! in the test process whose player renders to the null output, which
//! makes no sound and opens no device. See "Playback and jobs" and
//! "Events and subscriptions" in docs/design/contract.md.
//!
//! Nothing here sleeps: a test waits for an answer or an event, and is cut
//! off with a message if it has not finished in [`WAIT`].

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ritornel_client::Client;
use ritornel_client::contract::methods::{
    HistoryRecent, PlaybackGet, PlaybackPause, PlaybackPlay, PlaybackSkip, QueueWindow,
    SourcesTracks,
};
use ritornel_client::contract::{
    CONTRACT_VERSION, ClientKind, ErrorCode, Event, HelloParams, HistoryRecentParams, Id, NoParams,
    PlaySource, PlaybackCommandParams, PlaybackPlayParams, PlaybackSkipParams, PlaybackStatus,
    QueueWindowParams, SessionKind, SkipDirection, SourcesTracksParams, Topic, capability,
};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Output, PlaybackRun, Run, Server};

const WAIT: Duration = Duration::from_secs(60);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ritornel-playback-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Starts a server with a player on the null output, a hundred times real
/// time, and returns a client with a full session that has said hello.
async fn server(scratch: &Scratch) -> (Server, Client) {
    let run = Run {
        playback: PlaybackRun {
            output: Some(Output::Null { speed: 100 }),
            ..PlaybackRun::default()
        },
        ..Run::default()
    };
    let server = Server::start_run(
        HeldDir::take(scratch.0.join("data")).unwrap(),
        Listen::on_port(0),
        Arc::new(SystemClock),
        Arc::new(OsEntropy),
        Bounds::default(),
        run,
    )
    .await
    .unwrap();
    let (outgoing, incoming) = server.connect_in_memory(SessionKind::Full);
    let client = Client::over_memory(outgoing, incoming);
    let descriptor = client
        .hello(&HelloParams {
            name: "playback-test".to_owned(),
            kind: ClientKind::Cli,
            version: "1".to_owned(),
            contract_version: CONTRACT_VERSION,
        })
        .await
        .unwrap();
    assert!(
        descriptor
            .server
            .capabilities
            .contains(&capability::PLAYBACK.to_owned())
    );
    (server, client)
}

fn key() -> Id {
    ritornel_client::idempotency_key().unwrap()
}

/// An album of three tracks, from the audio fixtures, half a second each.
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

/// Creates the library and scans the folder into it, and waits for the
/// scan to end.
async fn scanned(client: &mut Client, folder: &Path) {
    client.create_library(key()).await.unwrap();
    let source = client
        .add_source(key(), "music", folder.to_str().unwrap())
        .await
        .unwrap()
        .source;
    client.subscribe(&[Topic::Jobs]).await.unwrap();
    let job = client
        .start_scan(key(), source.source_id)
        .await
        .unwrap()
        .job;
    loop {
        if let Event::Jobs(event) = client.next_event().await.unwrap()
            && event.job.job_id == job.job_id
            && event.job.state.ended()
        {
            break;
        }
    }
    client.unsubscribe(&[Topic::Jobs]).await.unwrap();
}

#[tokio::test]
async fn an_album_plays_to_its_end_and_the_player_says_so_on_its_topics() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let (server, mut client) = server(&scratch).await;
        let folder = album(&scratch);

        // Nothing to play before there is a library.
        let empty = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert_eq!(empty.status, PlaybackStatus::Stopped);
        assert_eq!(empty.output.name, "null");
        scanned(&mut client, &folder).await;

        let tracks = client
            .call::<SourcesTracks>(&SourcesTracksParams {
                path: folder.join("Tin Weather").to_str().unwrap().to_owned(),
            })
            .await
            .unwrap()
            .tracks;
        assert_eq!(tracks.len(), 3);
        // Played as a person reads the album: in disc and track order. The
        // fixtures all claim to be track 2 of one disc, so the files' order
        // decides.
        let refused = client
            .call::<SourcesTracks>(&SourcesTracksParams {
                path: "/not/a/root".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(refused, ritornel_client::Error::Call(ref error) if error.data.code == ErrorCode::NotFound),
            "{refused:?}"
        );

        client
            .subscribe(&[Topic::Playback, Topic::Queue, Topic::Progress])
            .await
            .unwrap();
        let first = key();
        let played = client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: first,
                source: PlaySource::Tracks {
                    ids: tracks.iter().map(|track| track.appearance_id).collect(),
                },
                start: None,
                context: None,
            })
            .await
            .unwrap();
        assert_eq!(played.receipt.command_id, first);
        assert_eq!(played.queue_revision, 1);
        let window = client
            .call::<QueueWindow>(&QueueWindowParams {
                offset: 0,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!((window.total, window.queue_revision), (3, 1));
        assert_eq!(window.entries[0].entry_id, played.entry_id.unwrap());

        // The queue changed, the player played, and in under a second of
        // the test's time the album has ended.
        let (mut queue_events, mut entries) = (0, Vec::new());
        loop {
            match client.next_event().await.unwrap() {
                Event::Queue(_) => queue_events += 1,
                Event::Playback(event) => {
                    if let Some(entry) = event.state.entry_id
                        && entries.last() != Some(&entry)
                    {
                        entries.push(entry);
                    }
                    if event.state.status == PlaybackStatus::Stopped && entries.len() == 3 {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(queue_events >= 1);
        let ids: Vec<Id> = window.entries.iter().map(|entry| entry.entry_id).collect();
        assert_eq!(entries, ids);

        // Sent again, the play command is answered from its receipt and
        // starts nothing.
        let again = client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: first,
                source: PlaySource::Tracks {
                    ids: tracks.iter().map(|track| track.appearance_id).collect(),
                },
                start: None,
                context: None,
            })
            .await
            .unwrap();
        assert_eq!(again, played);
        let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert_eq!(state.status, PlaybackStatus::Stopped);

        // A skip from an entry the player has left changes nothing.
        let skipped = client
            .call::<PlaybackSkip>(&PlaybackSkipParams {
                idempotency_key: key(),
                from_entry_id: ids[0],
                direction: SkipDirection::Next,
            })
            .await
            .unwrap();
        assert_eq!(skipped.entry_id, Some(ids[2]));
        client
            .call::<PlaybackPause>(&PlaybackCommandParams {
                idempotency_key: key(),
            })
            .await
            .unwrap();

        // Every listen reached history: three plays, each half a second of
        // a half-second track, none of which counts.
        let recent = loop {
            let recent = client
                .call::<HistoryRecent>(&HistoryRecentParams {
                    limit: None,
                    before: None,
                })
                .await
                .unwrap();
            if recent.undelivered == 0 && recent.plays.len() == 3 {
                break recent;
            }
            tokio::task::yield_now().await;
        };
        for play in &recent.plays {
            assert_eq!(play.ms_played, 500);
            assert!(!play.counts);
            assert_eq!(play.listens.len(), 1);
            assert_eq!(play.clock_basis, "hub_age");
        }
        // Read two at a time: a full page says where the next begins, and
        // the last says nothing more.
        let first_page = client
            .call::<HistoryRecent>(&HistoryRecentParams {
                limit: Some(2),
                before: None,
            })
            .await
            .unwrap();
        let next = first_page.next.unwrap();
        assert_eq!(next.play_id, recent.plays[1].play_id);
        let second_page = client
            .call::<HistoryRecent>(&HistoryRecentParams {
                limit: Some(2),
                before: Some(next),
            })
            .await
            .unwrap();
        assert_eq!(second_page.plays, recent.plays[2..]);
        assert_eq!(second_page.next, None);
        server.stop().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_file_replaced_after_the_scan_is_not_played_under_the_identity_the_scan_gave_the_old_one()
{
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let (server, mut client) = server(&scratch).await;
        let folder = album(&scratch);
        scanned(&mut client, &folder).await;
        let tracks = client
            .call::<SourcesTracks>(&SourcesTracksParams {
                path: folder.join("Tin Weather").to_str().unwrap().to_owned(),
            })
            .await
            .unwrap()
            .tracks;

        // Another song takes the first file's place, as a tagger or a copy
        // would put it there, and nothing has read it since.
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
        let first = folder.join("Tin Weather/01.flac");
        let incoming = folder.join("Tin Weather/.01.flac.part");
        std::fs::copy(fixtures.join("b.flac"), &incoming).unwrap();
        std::fs::rename(&incoming, &first).unwrap();

        let played = client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::Tracks {
                    ids: tracks.iter().map(|track| track.appearance_id).collect(),
                },
                start: None,
                context: None,
            })
            .await
            .unwrap();
        assert_eq!(played.queue_revision, 1);
        loop {
            let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
            if state.status == PlaybackStatus::Stopped {
                break;
            }
            tokio::task::yield_now().await;
        }
        let recent = loop {
            let recent = client
                .call::<HistoryRecent>(&HistoryRecentParams {
                    limit: None,
                    before: None,
                })
                .await
                .unwrap();
            if recent.undelivered == 0 {
                break recent;
            }
            tokio::task::yield_now().await;
        };

        // The first entry did not play: it is marked with why, and no listen
        // names the file the scan read there. The other two played.
        let window = client
            .call::<QueueWindow>(&QueueWindowParams {
                offset: 0,
                limit: None,
            })
            .await
            .unwrap();
        let failed: Vec<Option<&str>> = window
            .entries
            .iter()
            .map(|entry| entry.failed_reason.as_deref())
            .collect();
        assert_eq!(failed, [Some("file_changed"), None, None]);
        let heard: Vec<Option<Id>> = recent
            .plays
            .iter()
            .map(|play| play.track.appearance_id)
            .collect();
        assert_eq!(heard.len(), 2, "{heard:?}");
        assert!(!heard.contains(&Some(tracks[0].appearance_id)), "{heard:?}");
        server.stop().await.unwrap();
    })
    .await
    .unwrap();
}
