//! The player's calls and events through `ritornel-client`: a real server
//! in the test process whose player renders to the null output, which
//! makes no sound and opens no device. See "Commands, playback, and jobs" and
//! "Events and subscriptions" in docs/internals/contract.md.
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
use std::time::Duration;

use ritornel_client::Client;
use ritornel_client::contract::methods::{
    AlbumGet, HistoryRecent, ListClose, ListOpen, PlaybackGet, PlaybackPause, PlaybackPlay,
    PlaybackSet, PlaybackSkip, QueueAdd, QueueClear, QueueRemove, QueueWindow, RatingsSet,
    SourcesTracks,
};
use ritornel_client::contract::{
    AlbumGetParams, AlbumRef, CONTRACT_VERSION, ClientKind, ErrorCode, Event, HelloParams,
    HistoryRecentParams, Id, ListKind, ListOpenParams, NoParams, PlaySource, PlaybackCommandParams,
    PlaybackPlayParams, PlaybackSetParams, PlaybackSkipParams, PlaybackStatus, Query,
    QueueAddParams, QueueClearParams, QueueOrder, QueuePlace, QueueRemoveParams, QueueWindowParams,
    RatingSetParams, Repeat, Row, SessionKind, SkipDirection, SongRow, SourcesTracksParams, Topic,
    ViewParams, Window, capability,
};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Output, PlaybackRun, Run, Server};

const WAIT: Duration = Duration::from_secs(60);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = tempfile::Builder::new()
            .prefix("ritornel-playback-")
            .tempdir()
            .unwrap()
            .keep();
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
    server_at(scratch, 100).await
}

/// As [`server`], at `speed` times real time.
async fn server_at(scratch: &Scratch, speed: u32) -> (Server, Client) {
    let run = Run {
        playback: PlaybackRun {
            output: Some(Output::Null { speed }),
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
    scan_music(client, folder).await;
}

async fn scan_music(client: &mut Client, folder: &Path) {
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
async fn a_picture_folder_does_not_hide_tracks_in_a_music_folder() {
    for pictures_first in [true, false] {
        let scratch = Scratch::new();
        let (_server, mut client) = server_at(&scratch, 4).await;
        let music = album(&scratch);
        let parent = music.parent().unwrap();
        std::fs::write(parent.join("index.json"), r#"{"format":1,"artists":[]}"#).unwrap();
        client.create_library(key()).await.unwrap();
        let pictures = ritornel_contract::ImportPicturesParams {
            idempotency_key: key(),
            name: "portraits".into(),
            path: parent.display().to_string(),
        };
        if pictures_first {
            client
                .call::<ritornel_contract::methods::ArtistsImportPictures>(&pictures)
                .await
                .unwrap();
        }
        scan_music(&mut client, &music).await;
        if !pictures_first {
            client
                .call::<ritornel_contract::methods::ArtistsImportPictures>(&pictures)
                .await
                .unwrap();
        }
        let tracks = client
            .call::<SourcesTracks>(&SourcesTracksParams {
                path: music.display().to_string(),
            })
            .await
            .unwrap();
        assert_eq!(tracks.tracks.len(), 3);
        for folder in [parent, &music.join("Tin Weather")] {
            let err = client
                .add_source(key(), "other", folder.to_str().unwrap())
                .await
                .unwrap_err();
            assert!(
                matches!(err, ritornel_client::Error::Call(ref e) if e.data.code == ErrorCode::Invalid && e.message.contains(music.to_str().unwrap())),
                "{err}"
            );
        }
        let alias = music.join("Tin Weather").join("..");
        let err = client
            .add_source(key(), "music", alias.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ritornel_client::Error::Call(ref e) if e.data.code == ErrorCode::Conflict)
        );
        std::fs::create_dir(parent.join("sibling")).unwrap();
        let sibling = music.join("..").join("sibling");
        client
            .add_source(key(), "sibling", sibling.to_str().unwrap())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn an_album_plays_to_its_end_and_the_player_says_so_on_its_topics() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        // Four times real time: the album is over in well under a second.
        let (server, mut client) = server_at(&scratch, 4).await;
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
                // Absolute on every system, and under none of its roots.
                path: std::env::temp_dir()
                    .join("not-a-root")
                    .to_str()
                    .unwrap()
                    .to_owned(),
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
                shuffle: None,
            })
            .await
            .unwrap();
        assert_eq!(played.receipt.command_id, first);
        assert_eq!(played.queue_revision, 1);
        let window = client
            .call::<QueueWindow>(&QueueWindowParams {
                offset: 0,
                limit: None,
                order: None,
            })
            .await
            .unwrap();
        assert_eq!((window.total, window.queue_revision), (3, 1));
        assert_eq!(window.entries[0].entry_id, played.entry_id.unwrap());

        // The queue changed, the player played, and the album ended. An
        // event says the state as it is when sent, so a track can begin and
        // end between two when the machine is slow: those said are in the
        // queue's order, from the first to the last, and history below has
        // every one.
        let ids: Vec<Id> = window.entries.iter().map(|entry| entry.entry_id).collect();
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
                    if event.state.status == PlaybackStatus::Stopped
                        && entries.last() == ids.last()
                    {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(queue_events >= 1);
        let places: Vec<Option<usize>> = entries
            .iter()
            .map(|entry| ids.iter().position(|id| id == entry))
            .collect();
        assert_eq!(places.first(), Some(&Some(0)), "{places:?}");
        assert!(places.is_sorted() && !places.contains(&None), "{places:?}");

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
                shuffle: None,
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
            assert_eq!(play.ms_played, Some(500));
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
                shuffle: None,
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
                order: None,
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

#[tokio::test]
async fn the_volume_is_set_absolutely_kept_across_a_restart_and_refused_out_of_range() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let (first, client) = server(&scratch).await;
        assert_eq!(
            client.call::<PlaybackGet>(&NoParams {}).await.unwrap().volume,
            1.0
        );
        let set = PlaybackSetParams {
            idempotency_key: key(),
            volume: Some(0.25),
            muted: None,
            shuffle: None,
            repeat: None,
        };
        client.call::<PlaybackSet>(&set).await.unwrap();
        // Sent again, it is answered from its receipt.
        client.call::<PlaybackSet>(&set).await.unwrap();
        client
            .call::<PlaybackSet>(&PlaybackSetParams {
                idempotency_key: key(),
                volume: None,
                muted: Some(true),
                shuffle: None,
                repeat: None,
            })
            .await
            .unwrap();
        let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert_eq!((state.volume, state.muted), (0.25, true));
        let refused = client
            .call::<PlaybackSet>(&PlaybackSetParams {
                idempotency_key: key(),
                volume: Some(1.5),
                muted: None,
                shuffle: None,
                repeat: None,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(refused, ritornel_client::Error::Call(ref error) if error.data.code == ErrorCode::Invalid),
            "{refused:?}"
        );
        first.stop().await.unwrap();

        let (server, client) = server(&scratch).await;
        let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert_eq!((state.volume, state.muted), (0.25, true));
        server.stop().await.unwrap();
    })
    .await
    .unwrap();
}

/// What a call refused with: its code and its details.
fn refusal<T: std::fmt::Debug>(
    answer: Result<T, ritornel_client::Error>,
) -> (ErrorCode, serde_json::Value) {
    match answer {
        Err(ritornel_client::Error::Call(error)) => (
            error.data.code,
            serde_json::to_value(&error.data.details).unwrap(),
        ),
        other => panic!("expected an error from the server, and got {other:?}"),
    }
}

#[tokio::test]
async fn shuffle_repeat_queue_edits_and_stars_go_through_the_contract() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let (server, mut client) = server(&scratch).await;
        let folder = album(&scratch);
        scanned(&mut client, &folder).await;

        // Play from here: the second row of a view of every song.
        let opened = client
            .call::<ListOpen>(&ListOpenParams {
                kind: ListKind::Songs,
                query: Query::default(),
                window: Some(Window {
                    offset: 0,
                    limit: 10,
                }),
                replaces: None,
            })
            .await
            .unwrap();
        let songs: Vec<SongRow> = opened
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Song(song) => Some(song.clone()),
                _ => None,
            })
            .collect();
        assert!(!songs.is_empty());
        let last = songs.len() - 1;
        let played = client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::View {
                    view: opened.view.clone(),
                },
                start: Some(u32::try_from(last).unwrap()),
                context: None,
                shuffle: None,
            })
            .await
            .unwrap();
        let in_order = client
            .call::<QueueWindow>(&QueueWindowParams {
                offset: 0,
                limit: None,
                order: None,
            })
            .await
            .unwrap();
        // The view's rows, in its order, from the row asked for.
        assert_eq!(played.entry_id, Some(in_order.entries[last].entry_id));
        let queued: Vec<Id> = in_order
            .entries
            .iter()
            .map(|entry| entry.track.appearance_id)
            .collect();
        let listed: Vec<Id> = songs.iter().map(|song| song.appearance_id).collect();
        assert_eq!(queued, listed);

        // The album's three tracks, paused, for the rest.
        let songs = client
            .call::<SourcesTracks>(&SourcesTracksParams {
                path: folder.join("Tin Weather").to_str().unwrap().to_owned(),
            })
            .await
            .unwrap()
            .tracks;
        let played = client
            .call::<PlaybackPlay>(&PlaybackPlayParams {
                idempotency_key: key(),
                source: PlaySource::Tracks {
                    ids: songs.iter().map(|song| song.appearance_id).collect(),
                },
                start: Some(1),
                context: None,
                shuffle: None,
            })
            .await
            .unwrap();
        client
            .call::<PlaybackPause>(&PlaybackCommandParams {
                idempotency_key: key(),
            })
            .await
            .unwrap();
        let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert_eq!(state.queue_length, 3);

        // Shuffle on and repeat all, said back in the state.
        let set = client
            .call::<PlaybackSet>(&PlaybackSetParams {
                idempotency_key: key(),
                volume: None,
                muted: None,
                shuffle: Some(true),
                repeat: Some(Repeat::All),
            })
            .await
            .unwrap();
        assert_eq!(set.queue_revision, played.queue_revision + 1);
        let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert!(state.shuffle);
        assert_eq!(state.repeat, Repeat::All);
        let window = |order| QueueWindowParams {
            offset: 0,
            limit: None,
            order: Some(order),
        };
        let play_order = client
            .call::<QueueWindow>(&window(QueueOrder::Play))
            .await
            .unwrap();
        assert_eq!(play_order.entries[0].entry_id, state.entry_id.unwrap());
        assert_eq!(play_order.current_index, Some(0));
        let up_next = client
            .call::<QueueWindow>(&window(QueueOrder::UpNext))
            .await
            .unwrap();
        assert_eq!(up_next.total, 2);
        assert_eq!(up_next.entries, play_order.entries[1..]);

        // An edit decided on an old revision is refused, and says the
        // revision there is.
        let stale = client
            .call::<QueueAdd>(&QueueAddParams {
                idempotency_key: key(),
                source: PlaySource::Tracks {
                    ids: vec![songs[0].appearance_id],
                },
                place: QueuePlace::Next,
                context: None,
                expected_revision: Some(played.queue_revision),
                play_if_empty: None,
            })
            .await;
        let (code, details) = refusal(stale);
        assert_eq!(code, ErrorCode::Conflict);
        assert_eq!(details["currentRevision"], set.queue_revision);
        // On the revision there is, it goes straight after the one on.
        let added = client
            .call::<QueueAdd>(&QueueAddParams {
                idempotency_key: key(),
                source: PlaySource::Tracks {
                    ids: vec![songs[0].appearance_id],
                },
                place: QueuePlace::Next,
                context: None,
                expected_revision: Some(set.queue_revision),
                play_if_empty: None,
            })
            .await
            .unwrap();
        assert_eq!(added.queue_revision, set.queue_revision + 1);
        let up_next = client
            .call::<QueueWindow>(&window(QueueOrder::UpNext))
            .await
            .unwrap();
        assert_eq!(up_next.total, 3);
        assert_eq!(
            up_next.entries[0].track.appearance_id,
            songs[0].appearance_id
        );
        let removed = client
            .call::<QueueRemove>(&QueueRemoveParams {
                idempotency_key: key(),
                entry_ids: vec![up_next.entries[0].entry_id],
                expected_revision: None,
            })
            .await
            .unwrap();
        assert_eq!(removed.queue_revision, added.queue_revision + 1);

        // Stars on the song on show in the state, and on its row.
        let recording = state.track.as_ref().unwrap().recording_id.unwrap();
        let rated = client
            .call::<RatingsSet>(&RatingSetParams {
                idempotency_key: key(),
                recording_id: recording,
                rating: Some(80),
            })
            .await
            .unwrap();
        assert_eq!(rated.rating, Some(80));
        assert!(rated.receipt.revision.is_some());
        let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert_eq!(state.track.as_ref().unwrap().rating, Some(80));
        let page = client
            .call::<AlbumGet>(&AlbumGetParams {
                album: AlbumRef::Release {
                    id: state.track.as_ref().unwrap().release_id.unwrap(),
                },
            })
            .await
            .unwrap();
        let stars: Vec<Option<u32>> = page
            .tracks
            .iter()
            .filter(|track| track.recording_id == recording)
            .map(|track| track.rating)
            .collect();
        // Every appearance of the recording shows its stars.
        assert!(!stars.is_empty());
        assert!(stars.iter().all(|stars| *stars == Some(80)), "{stars:?}");
        let refused = client
            .call::<RatingsSet>(&RatingSetParams {
                idempotency_key: key(),
                recording_id: recording,
                rating: Some(120),
            })
            .await;
        assert_eq!(refusal(refused).0, ErrorCode::Invalid);

        // Clearing stops the player and empties the queue.
        client
            .call::<QueueClear>(&QueueClearParams {
                idempotency_key: key(),
                expected_revision: None,
            })
            .await
            .unwrap();
        let state = client.call::<PlaybackGet>(&NoParams {}).await.unwrap();
        assert_eq!(
            (state.status, state.queue_length),
            (PlaybackStatus::Stopped, 0)
        );
        server.stop().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_command_from_a_view_sent_again_after_the_view_ended_is_answered_from_its_receipt() {
    tokio::time::timeout(WAIT, async {
        let scratch = Scratch::new();
        let (server, mut client) = server(&scratch).await;
        let folder = album(&scratch);
        scanned(&mut client, &folder).await;
        let opened = client
            .call::<ListOpen>(&ListOpenParams {
                kind: ListKind::Songs,
                query: Query::default(),
                window: None,
                replaces: None,
            })
            .await
            .unwrap();
        let view = || PlaySource::View {
            view: opened.view.clone(),
        };
        let play = PlaybackPlayParams {
            idempotency_key: key(),
            source: view(),
            start: Some(0),
            context: None,
            shuffle: None,
        };
        let played = client.call::<PlaybackPlay>(&play).await.unwrap();
        let add = QueueAddParams {
            idempotency_key: key(),
            source: view(),
            place: QueuePlace::End,
            context: None,
            expected_revision: None,
            play_if_empty: None,
        };
        let added = client.call::<QueueAdd>(&add).await.unwrap();
        client
            .call::<ListClose>(&ViewParams {
                view: opened.view.clone(),
            })
            .await
            .unwrap();
        // The view has ended: what was done with it is still answered.
        assert_eq!(client.call::<PlaybackPlay>(&play).await.unwrap(), played);
        assert_eq!(client.call::<QueueAdd>(&add).await.unwrap(), added);
        server.stop().await.unwrap();
    })
    .await
    .unwrap();
}
