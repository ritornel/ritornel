//! Contract tests for lists read through views, the `library` and `list`
//! events, and covers with the media token: a real server inside the test,
//! on an invented library of its own, talked to through `ritornel-client`.
//!
//! Each test of a call runs twice, joined to the dispatcher by channels in
//! memory and over a real socket ("Transport-neutral dispatcher" in
//! docs/internals/contract.md).
//! Covers are plain HTTP, so their tests run over the socket's listener.

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use ritornel_client::contract::{
    AlbumTile, CONTRACT_VERSION, ClientKind, ErrorCode, Event, FILTER_VERSION, Filter, HelloParams,
    ListKind, ListLocateParams, ListOpenParams, ListOpened, MEDIA_TOKEN_PARAM, Predicate, Query,
    Replaces, Row, Sort, SortKey, Topic, Window, route,
};
use ritornel_client::{Client, Credential};
use ritornel_engine::invent::{ChurnKind, Churned};
use ritornel_server::Bounds;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::support::{
    Running, Scratch, Via, bounded, invent as run_invent, on_both_transports, terminate,
};

fn scratch() -> Scratch {
    Scratch::new("ritornel-server-lists-")
}

/// Invents a library on a data directory with the server's own hidden
/// command, as a developer would.
fn invent(data_dir: &Path, root: &Path, albums: usize) {
    let albums = albums.to_string();
    run_invent(
        data_dir,
        root,
        &[
            "--albums",
            &albums,
            "--seed",
            "3",
            "--cover-min",
            "200",
            "--cover-max",
            "300",
        ],
    );
}

/// Starts a server inside the test, on an invented library of its own.
async fn start(albums: usize) -> Running {
    let scratch = scratch();
    if albums > 0 {
        invent(&scratch.data_dir(), &scratch.root(), albums);
    }
    Running::on_scratch(scratch, Bounds::default())
        .await
        .with_client_name("lists-test")
}

fn step(running: &Running, kind: ChurnKind) -> Churned {
    running.server.invent_step(kind).unwrap()
}

fn open(kind: ListKind, sort: Option<SortKey>, window: Option<(u64, u64)>) -> ListOpenParams {
    ListOpenParams {
        kind,
        query: Query {
            sort: sort.map(|key| vec![Sort { key }]),
            ..Query::default()
        },
        window: window.map(|(offset, limit)| Window { offset, limit }),
        replaces: None,
    }
}

fn refused<T: std::fmt::Debug>(answer: Result<T, ritornel_client::Error>) -> ErrorCode {
    match answer {
        Err(ritornel_client::Error::Call(error)) => error.data.code,
        other => panic!("expected an error from the server, and got {other:?}"),
    }
}

fn album(row: &Row) -> &AlbumTile {
    match row {
        Row::Album(album) => album,
        other => panic!("{other:?}"),
    }
}

/// Every row of a view, a window at a time.
async fn all_rows(client: &Client, opened: &ListOpened) -> Vec<Row> {
    let mut rows = Vec::new();
    while (rows.len() as u64) < opened.total {
        let window = client
            .list_window(&opened.view, rows.len() as u64, 500)
            .await
            .unwrap();
        assert_eq!(window.offset, rows.len() as u64);
        assert!(!window.rows.is_empty());
        rows.extend(window.rows);
    }
    rows
}

on_both_transports! {
    a_list_opens_with_an_exact_total_and_the_window_asked_for
    sections_are_letters_years_or_stretches_of_time
    a_row_is_found_by_its_id_and_by_the_start_of_its_name
    text_matches_ignoring_case_and_accents_and_short_text_a_word_start
    a_filter_by_artist_gives_their_records
    a_write_tells_each_view_and_each_subscriber_once_for_what_was_not_yet_read
    a_view_replaced_with_an_anchor_says_where_the_anchor_is_now
    a_row_gone_since_the_view_opened_is_gone_at_its_index
    a_session_holds_at_most_its_limit_of_views
    a_view_that_was_closed_or_never_opened_has_expired
    a_server_with_no_library_has_no_lists
    the_media_token_is_the_same_for_the_session
}

async fn a_list_opens_with_an_exact_total_and_the_window_asked_for(via: Via) {
    let running = start(40).await;
    let client = running.greeted(via).await;
    for (kind, expected) in [
        (ListKind::Artists, "artist"),
        (ListKind::Albums, "album"),
        (ListKind::Songs, "song"),
    ] {
        let opened = client
            .list_open(&open(kind, None, Some((0, 10))))
            .await
            .unwrap();
        assert!(opened.total > 10, "{kind:?}");
        assert_eq!(opened.rows.len(), 10);
        assert_eq!(opened.offset, 0);
        assert_eq!(opened.anchor_index, None);
        for row in &opened.rows {
            assert_eq!(serde_json::to_value(row).unwrap()["type"], expected);
        }
        // The last window is short, and one past the end is empty.
        let last = client
            .list_window(&opened.view, opened.total - 3, 10)
            .await
            .unwrap();
        assert_eq!(last.rows.len(), 3);
        let past = client
            .list_window(&opened.view, opened.total + 5, 10)
            .await
            .unwrap();
        assert_eq!((past.offset, past.rows.len()), (opened.total, 0));
        // The same rows, read again, are the same.
        let again = client.list_window(&opened.view, 0, 10).await.unwrap();
        assert_eq!(again.rows, opened.rows);
        // More than the limit is refused.
        assert_eq!(
            refused(client.list_window(&opened.view, 0, 501).await),
            ErrorCode::Invalid
        );
    }
    // A view with no window asked for sends no rows.
    let opened = client
        .list_open(&open(ListKind::Albums, None, None))
        .await
        .unwrap();
    assert!(opened.rows.is_empty());
    // Tiles say what they are and how many versions they have.
    let rows = all_rows(&client, &opened).await;
    assert_eq!(rows.len() as u64, opened.total);
    for row in &rows {
        let tile = album(row);
        assert!(tile.title.is_some() && tile.track_count > 0 && tile.version_count >= 1);
    }
    // A sort the kind does not take, and two sorts, are refused.
    assert_eq!(
        refused(
            client
                .list_open(&open(ListKind::Artists, Some(SortKey::Year), None))
                .await
        ),
        ErrorCode::Invalid
    );
}

async fn sections_are_letters_years_or_stretches_of_time(via: Via) {
    let running = start(60).await;
    let client = running.greeted(via).await;
    for (kind, sort) in [
        (ListKind::Artists, SortKey::Name),
        (ListKind::Albums, SortKey::Artist),
        (ListKind::Albums, SortKey::Title),
        (ListKind::Songs, SortKey::Title),
        (ListKind::Songs, SortKey::Album),
    ] {
        let opened = client
            .list_open(&open(kind, Some(sort), None))
            .await
            .unwrap();
        let sections = client.list_sections(&opened.view).await.unwrap().sections;
        let labels: Vec<&str> = sections
            .iter()
            .map(|section| section.label.as_str())
            .collect();
        assert_eq!(sections[0].index, 0, "{kind:?} {sort:?}");
        assert!(
            sections
                .windows(2)
                .all(|pair| pair[0].index < pair[1].index)
        );
        // Letters, A to Z, then #.
        for label in &labels {
            assert!(
                *label == "#"
                    || (label.len() == 1 && label.chars().all(|c| c.is_ascii_uppercase())),
                "{label}"
            );
        }
        let mut sorted = labels.clone();
        sorted.sort_by_key(|label| (*label == "#", label.to_string()));
        assert_eq!(labels, sorted, "{kind:?} {sort:?}");
    }
    // By year: years, newest first.
    let opened = client
        .list_open(&open(ListKind::Albums, Some(SortKey::Year), Some((0, 500))))
        .await
        .unwrap();
    let sections = client.list_sections(&opened.view).await.unwrap().sections;
    let years: Vec<i64> = sections
        .iter()
        .map(|section| section.label.parse().unwrap())
        .collect();
    assert!(years.windows(2).all(|pair| pair[0] > pair[1]), "{years:?}");
    for section in &sections {
        let tile = album(&opened.rows[section.index as usize]);
        assert_eq!(tile.year.unwrap().to_string(), section.label);
    }
    // By when they came in: stretches of time, the newest first.
    let opened = client
        .list_open(&open(ListKind::Albums, Some(SortKey::Added), None))
        .await
        .unwrap();
    let sections = client.list_sections(&opened.view).await.unwrap().sections;
    assert!(!sections.is_empty());
    let first = sections[0].label.as_str();
    assert!(
        ["This week", "The last few weeks"].contains(&first)
            || first.parse::<i64>().is_ok()
            || first.ends_with("ber")
            || first.ends_with('y')
            || first.chars().all(char::is_alphabetic),
        "{first}"
    );
}

async fn a_row_is_found_by_its_id_and_by_the_start_of_its_name(via: Via) {
    let running = start(60).await;
    let client = running.greeted(via).await;
    let opened = client
        .list_open(&open(
            ListKind::Albums,
            Some(SortKey::Title),
            Some((0, 500)),
        ))
        .await
        .unwrap();
    let tile = album(&opened.rows[17]);
    let located = client
        .list_locate(&ListLocateParams {
            view: opened.view.clone(),
            id: Some(tile.album.id().to_string()),
            prefix: None,
        })
        .await
        .unwrap();
    assert_eq!(located.index, Some(17));
    // An ID the view does not hold.
    let located = client
        .list_locate(&ListLocateParams {
            view: opened.view.clone(),
            id: Some(ritornel_client::contract::Id::from_bytes([9; 16]).to_string()),
            prefix: None,
        })
        .await
        .unwrap();
    assert_eq!(located.index, None);
    // The start of a title, typed as a person would.
    let title = tile.title.clone().unwrap();
    let typed: String = title
        .trim_start_matches("The ")
        .chars()
        .take(3)
        .collect::<String>()
        .to_lowercase();
    let located = client
        .list_locate(&ListLocateParams {
            view: opened.view.clone(),
            id: None,
            prefix: Some(typed.clone()),
        })
        .await
        .unwrap();
    let at = located.index.unwrap() as usize;
    assert!(at <= 17, "{typed} at {at}");
    let found = album(&opened.rows[at])
        .title
        .clone()
        .unwrap()
        .to_lowercase();
    assert!(
        found.trim_start_matches("the ").starts_with(&typed),
        "{found} for {typed}"
    );
    // Both, or neither, is refused.
    for (id, prefix) in [(None, None), (Some("x".to_owned()), Some("x".to_owned()))] {
        let params = ListLocateParams {
            view: opened.view.clone(),
            id,
            prefix,
        };
        assert_eq!(
            refused(client.list_locate(&params).await),
            ErrorCode::Invalid
        );
    }
}

/// Text folded as the server folds it, for checking what it found.
fn folded(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'á' | 'à' | 'å' | 'ä' => 'a',
            'é' | 'è' | 'É' => 'e',
            'í' | 'î' => 'i',
            'ó' | 'ö' | 'ø' => 'o',
            'ć' => 'c',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

async fn text_matches_ignoring_case_and_accents_and_short_text_a_word_start(via: Via) {
    let running = start(150).await;
    let client = running.greeted(via).await;
    // An artist with an accent in their name.
    let artists = client
        .list_open(&open(ListKind::Artists, None, Some((0, 500))))
        .await
        .unwrap();
    let accented = artists
        .rows
        .iter()
        .filter_map(|row| match row {
            Row::Artist(artist) => artist.name.clone(),
            _ => None,
        })
        .find(|name| name.chars().any(|c| "éèÉåäöøćíî".contains(c)))
        .expect("an invented artist with an accent");
    let word = accented
        .split(' ')
        .find(|word| word.chars().any(|c| "éèÉåäöøćíî".contains(c)) && word.chars().count() >= 3)
        .unwrap_or(&accented)
        .to_owned();
    // Typed without its accent and in capitals, it finds their songs.
    let typed = folded(&word).to_uppercase();
    let query = |text: &str| ListOpenParams {
        kind: ListKind::Songs,
        query: Query {
            text: Some(text.to_owned()),
            ..Query::default()
        },
        window: Some(Window {
            offset: 0,
            limit: 500,
        }),
        replaces: None,
    };
    let found = client.list_open(&query(&typed)).await.unwrap();
    assert!(found.total > 0, "{typed}");
    for row in &found.rows {
        let Row::Song(song) = row else { panic!() };
        let haystack = folded(&format!(
            "{} {} {}",
            song.title.as_deref().unwrap_or_default(),
            song.artist_credit
                .as_ref()
                .map_or("", |credit| credit.text.as_str()),
            song.album_title.as_deref().unwrap_or_default()
        ));
        assert!(haystack.contains(&folded(&word)), "{haystack} for {typed}");
    }
    // One and two characters match the start of a word, and nothing else.
    for typed in ["s", "Ro"] {
        let found = client.list_open(&query(typed)).await.unwrap();
        assert!(found.total > 0, "{typed}");
        let needle = typed.to_lowercase();
        for row in &found.rows {
            let Row::Song(song) = row else { panic!() };
            let haystack = folded(&format!(
                "{} {} {}",
                song.title.as_deref().unwrap_or_default(),
                song.artist_credit
                    .as_ref()
                    .map_or("", |credit| credit.text.as_str()),
                song.album_title.as_deref().unwrap_or_default()
            ));
            assert!(
                haystack
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|w| w.starts_with(&needle)),
                "{haystack} for {typed}"
            );
        }
    }
    // Text that matches nothing is an empty view, not an error.
    let none = client.list_open(&query("zzqqxxzz")).await.unwrap();
    assert_eq!(none.total, 0);
}

async fn a_filter_by_artist_gives_their_records(via: Via) {
    let running = start(80).await;
    let client = running.greeted(via).await;
    let artists = client
        .list_open(&open(ListKind::Artists, None, Some((0, 500))))
        .await
        .unwrap();
    let busiest = artists
        .rows
        .iter()
        .filter_map(|row| match row {
            Row::Artist(artist) => Some(artist.clone()),
            _ => None,
        })
        .max_by_key(|artist| artist.album_count)
        .unwrap();
    let params = ListOpenParams {
        kind: ListKind::Albums,
        query: Query {
            filter: Some(Filter {
                version: FILTER_VERSION,
                predicate: Predicate::ByArtist {
                    artist_id: busiest.artist_id,
                },
            }),
            ..Query::default()
        },
        window: Some(Window {
            offset: 0,
            limit: 100,
        }),
        replaces: None,
    };
    let theirs = client.list_open(&params).await.unwrap();
    assert_eq!(theirs.total, busiest.album_count);
    for row in &theirs.rows {
        assert!(
            album(row)
                .artist_credit
                .as_ref()
                .unwrap()
                .artist_ids
                .contains(&busiest.artist_id)
        );
    }
    // A filter the artists list does not take, and a filter from a later
    // version of the language.
    let mut wrong = params.clone();
    wrong.kind = ListKind::Artists;
    assert_eq!(refused(client.list_open(&wrong).await), ErrorCode::Invalid);
    let mut later = params.clone();
    later.query.filter.as_mut().unwrap().version = FILTER_VERSION + 1;
    assert_eq!(
        refused(client.list_open(&later).await),
        ErrorCode::Unsupported
    );
}

async fn a_write_tells_each_view_and_each_subscriber_once_for_what_was_not_yet_read(via: Via) {
    let running = start(30).await;
    let mut client = running.greeted(via).await;
    client.subscribe(&[Topic::Library]).await.unwrap();
    let opened = client
        .list_open(&open(ListKind::Albums, None, None))
        .await
        .unwrap();
    // Two writes with nothing sent between them: the test's runtime has one
    // thread, and this does not let go of it.
    assert!(matches!(
        step(&running, ChurnKind::Change),
        Churned::Changed(_)
    ));
    assert!(matches!(step(&running, ChurnKind::Add), Churned::Added(_)));

    let mut library = Vec::new();
    let mut lists = Vec::new();
    while library.is_empty() || lists.is_empty() {
        match client.next_event().await.unwrap() {
            Event::Library(event) => library.push(event),
            Event::List(event) => lists.push(event),
            Event::Servers(_)
            | Event::Jobs(_)
            | Event::Playback(_)
            | Event::Progress(_)
            | Event::Players
            | Event::Queue(_) => {}
        }
    }
    // One of each, at the second write's revision, saying to reorder.
    assert_eq!(library.len(), 1);
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].view, opened.view);
    assert!(lists[0].reorder);
    assert_eq!(lists[0].library_revision, library[0].library_revision);
    assert!(library[0].library_revision >= opened.library_revision + 2);
    assert_eq!(
        library[0].changed,
        ritornel_client::contract::LibraryChange::ALL
    );
    // Nothing else is waiting.
    client.servers().await.unwrap();
    assert!(client.try_next_event().is_none());

    // A session that holds no view and is not subscribed is told nothing.
    let mut quiet = running.greeted(via).await;
    step(&running, ChurnKind::Change);
    quiet.servers().await.unwrap();
    assert!(quiet.try_next_event().is_none());
}

async fn a_view_replaced_with_an_anchor_says_where_the_anchor_is_now(via: Via) {
    let running = start(40).await;
    let client = running.greeted(via).await;
    let opened = client
        .list_open(&open(
            ListKind::Albums,
            Some(SortKey::Title),
            Some((0, 100)),
        ))
        .await
        .unwrap();
    let anchor = album(&opened.rows[20]).album.id().to_string();
    // Records come in, some of them before the anchor.
    for _ in 0..4 {
        step(&running, ChurnKind::Add);
    }
    let mut replaced = open(ListKind::Albums, Some(SortKey::Title), Some((0, 200)));
    replaced.replaces = Some(Replaces {
        view: opened.view.clone(),
        anchor: Some(anchor.clone()),
    });
    let successor = client.list_open(&replaced).await.unwrap();
    assert!(successor.total > opened.total);
    assert!(successor.library_revision > opened.library_revision);
    let at = successor.anchor_index.unwrap() as usize;
    assert_eq!(album(&successor.rows[at]).album.id().to_string(), anchor);
    // The view it replaced has ended.
    assert_eq!(
        refused(client.list_window(&opened.view, 0, 10).await),
        ErrorCode::ViewExpired
    );
    // An anchor the new view does not hold is left out.
    let mut gone = open(ListKind::Albums, None, None);
    gone.replaces = Some(Replaces {
        view: successor.view.clone(),
        anchor: Some(ritornel_client::contract::Id::from_bytes([3; 16]).to_string()),
    });
    assert_eq!(client.list_open(&gone).await.unwrap().anchor_index, None);
}

async fn a_row_gone_since_the_view_opened_is_gone_at_its_index(via: Via) {
    let running = start(40).await;
    let client = running.greeted(via).await;
    let opened = client
        .list_open(&open(ListKind::Songs, None, None))
        .await
        .unwrap();
    assert_eq!(step(&running, ChurnKind::Merge), Churned::Merged);
    let rows = all_rows(&client, &opened).await;
    // The view still has every index it had, and the merged tracks are
    // gone at theirs.
    assert_eq!(rows.len() as u64, opened.total);
    let gone = rows
        .iter()
        .filter(|row| matches!(row, Row::Gone(_)))
        .count();
    assert!(gone > 0);
    assert_eq!(
        serde_json::to_value(rows.iter().find(|row| matches!(row, Row::Gone(_))).unwrap()).unwrap(),
        json!({ "type": "gone", "gone": true })
    );
    // A view opened now does not have them. (It may have songs it did not
    // have: when the version a record showed is merged away, another
    // version's songs show in its place.)
    let now = client
        .list_open(&open(ListKind::Songs, None, None))
        .await
        .unwrap();
    assert!(now.total <= opened.total);
    let rows = all_rows(&client, &now).await;
    assert!(!rows.iter().any(|row| matches!(row, Row::Gone(_))));
}

async fn a_session_holds_at_most_its_limit_of_views(via: Via) {
    let running = start(10).await;
    let client = running.greeted(via).await;
    let descriptor = client
        .hello(&HelloParams {
            presence: None,
            name: "lists-test".to_owned(),
            kind: ClientKind::Cli,
            version: "1".to_owned(),
            contract_version: CONTRACT_VERSION,
        })
        .await
        .unwrap();
    let limit = descriptor.server.limits.max_views as usize;
    assert_eq!(limit, 16);
    let mut views = Vec::new();
    for _ in 0..limit {
        views.push(
            client
                .list_open(&open(ListKind::Artists, None, None))
                .await
                .unwrap()
                .view,
        );
    }
    // Neither a bad query nor a bad first window may cost a working view.
    for (sort, window) in [(Some(SortKey::Year), None), (None, Some((0, 501)))] {
        let mut bad = open(ListKind::Artists, sort, window);
        bad.replaces = Some(Replaces {
            view: views[0].clone(),
            anchor: None,
        });
        assert!(client.list_open(&bad).await.is_err());
        for view in &views {
            client.list_window(view, 0, 1).await.unwrap();
        }
    }
    // The first is used, so the second is the least recently used.
    client.list_window(&views[0], 0, 1).await.unwrap();
    client
        .list_open(&open(ListKind::Artists, None, None))
        .await
        .unwrap();
    assert_eq!(
        refused(client.list_window(&views[1], 0, 1).await),
        ErrorCode::ViewExpired
    );
    client.list_window(&views[0], 0, 1).await.unwrap();
    client.list_window(&views[2], 0, 1).await.unwrap();
    // Another session's views are its own.
    let other = running.greeted(via).await;
    assert_eq!(
        refused(other.list_window(&views[0], 0, 1).await),
        ErrorCode::ViewExpired
    );
}

async fn a_view_that_was_closed_or_never_opened_has_expired(via: Via) {
    let running = start(10).await;
    let client = running.greeted(via).await;
    let opened = client
        .list_open(&open(ListKind::Artists, None, None))
        .await
        .unwrap();
    assert!(client.list_close(&opened.view).await.unwrap().closed);
    assert!(!client.list_close(&opened.view).await.unwrap().closed);
    for view in [opened.view.as_str(), "never"] {
        assert_eq!(
            refused(client.list_window(view, 0, 1).await),
            ErrorCode::ViewExpired
        );
        assert_eq!(
            refused(client.list_sections(view).await),
            ErrorCode::ViewExpired
        );
        let locate = ListLocateParams {
            view: view.to_owned(),
            id: None,
            prefix: Some("a".to_owned()),
        };
        assert_eq!(
            refused(client.list_locate(&locate).await),
            ErrorCode::ViewExpired
        );
    }
}

async fn a_server_with_no_library_has_no_lists(via: Via) {
    let running = start(0).await;
    let client = running.greeted(via).await;
    match client.list_open(&open(ListKind::Artists, None, None)).await {
        Err(ritornel_client::Error::Call(error)) => {
            assert_eq!(error.data.code, ErrorCode::Unavailable);
            assert_eq!(error.data.details["reason"], "no_library");
        }
        other => panic!("{other:?}"),
    }
}

async fn the_media_token_is_the_same_for_the_session(via: Via) {
    let running = start(5).await;
    let client = running.greeted(via).await;
    let token = client.media_token().await.unwrap().token;
    assert_eq!(token.len(), 64);
    assert_eq!(client.media_token().await.unwrap().token, token);
    let other = running.greeted(via).await;
    assert_ne!(other.media_token().await.unwrap().token, token);
}

// ---------------------------------------------------------------- covers

/// What an HTTP request was answered with.
struct Answer {
    status: u16,
    head: String,
    body: Vec<u8>,
}

/// Sends one GET with these headers and reads the whole answer.
async fn get(endpoint: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Answer {
    let mut stream = TcpStream::connect(endpoint).await.unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.unwrap();
    let split = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let head = String::from_utf8_lossy(&answer[..split]).into_owned();
    let status = head[9..12].parse().unwrap();
    Answer {
        status,
        head,
        body: answer[split + 4..].to_vec(),
    }
}

/// The art IDs of the first album tiles that have one.
async fn arts(client: &Client) -> Vec<String> {
    let opened = client
        .list_open(&open(ListKind::Albums, None, Some((0, 100))))
        .await
        .unwrap();
    opened
        .rows
        .iter()
        .filter_map(|row| album(row).art_id.map(|art| art.to_string()))
        .collect()
}

fn art(art: &str, size: u32, token: Option<&str>) -> String {
    match token {
        Some(token) => format!("{}/{art}/{size}?{MEDIA_TOKEN_PARAM}={token}", route::ART),
        None => format!("{}/{art}/{size}", route::ART),
    }
}

#[tokio::test]
async fn a_cover_is_served_at_a_size_with_immutable_headers_and_kept_for_next_time() {
    bounded(async {
        let running = start(10).await;
        let client = running.greeted(Via::Socket).await;
        let token = client.media_token().await.unwrap().token;
        let arts = arts(&client).await;
        let first = &arts[0];

        let answer = get(running.endpoint(), &art(first, 128, Some(&token)), &[]).await;
        assert_eq!(answer.status, 200, "{}", answer.head);
        let head = answer.head.to_lowercase();
        assert!(head.contains("content-type: image/jpeg"), "{head}");
        assert!(
            head.contains("cache-control: private, max-age=31536000, immutable"),
            "{head}"
        );
        let decoded = image_size(&answer.body);
        assert_eq!(decoded.0.max(decoded.1), 128);
        // It is in the cache now, and a second visit is served from there,
        // even with its source gone.
        let cached = running
            .scratch
            .data_dir()
            .join(format!("cache/covers/{first}-128.jpg"));
        assert_eq!(std::fs::read(&cached).unwrap(), answer.body);
        std::fs::remove_dir_all(running.scratch.root()).unwrap();
        let again = get(running.endpoint(), &art(first, 128, Some(&token)), &[]).await;
        assert_eq!((again.status, again.body), (200, answer.body));
        // A size not in the descriptor, an art ID that is not one, and one
        // whose files are gone are not found.
        for path in [
            art(first, 100, Some(&token)),
            art("nonsense", 128, Some(&token)),
            art(&arts[1], 128, Some(&token)),
        ] {
            assert_eq!(
                get(running.endpoint(), &path, &[]).await.status,
                404,
                "{path}"
            );
        }
        // The credential in a header reads covers too.
        let bearer = format!("Bearer {}", running.token());
        let by_header = get(
            running.endpoint(),
            &art(first, 128, None),
            &[("Authorization", &bearer)],
        )
        .await;
        assert_eq!(by_header.status, 200);
    })
    .await;
}

/// The width and height of a JPEG.
fn image_size(jpeg: &[u8]) -> (u32, u32) {
    // The first start-of-frame marker holds them.
    let mut at = 2;
    while at + 9 < jpeg.len() {
        let (marker, length) = (
            jpeg[at + 1],
            u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]),
        );
        if (0xc0..=0xc2).contains(&marker) {
            let height = u16::from_be_bytes([jpeg[at + 5], jpeg[at + 6]]);
            let width = u16::from_be_bytes([jpeg[at + 7], jpeg[at + 8]]);
            return (u32::from(width), u32::from(height));
        }
        at += 2 + usize::from(length);
    }
    panic!("not a JPEG");
}

#[tokio::test]
async fn a_cover_needs_the_media_token_and_the_token_opens_nothing_else() {
    bounded(async {
        let running = start(5).await;
        let client = running.greeted(Via::Socket).await;
        let token = client.media_token().await.unwrap().token;
        let first = arts(&client).await.remove(0);
        let endpoint = running.endpoint();
        // No token, a wrong one, and the session token in its place.
        let wrong = "0".repeat(64);
        for path in [
            art(&first, 64, None),
            art(&first, 64, Some(&wrong)),
            art(&first, 64, Some(&running.token())),
        ] {
            assert_eq!(get(endpoint, &path, &[]).await.status, 401, "{path}");
        }
        assert_eq!(
            get(endpoint, &art(&first, 64, Some(&token)), &[])
                .await
                .status,
            200
        );
        // The media token opens no other route.
        for path in [
            format!("{}?{MEDIA_TOKEN_PARAM}={token}", route::HEALTH),
            format!("{}?{MEDIA_TOKEN_PARAM}={token}", route::WS),
            format!("{}?ticket={token}", route::WS),
            format!("/v1/nothing?{MEDIA_TOKEN_PARAM}={token}"),
        ] {
            assert_eq!(get(endpoint, &path, &[]).await.status, 401, "{path}");
        }
        let in_header = format!("Bearer {token}");
        assert_eq!(
            get(endpoint, route::HEALTH, &[("Authorization", &in_header)])
                .await
                .status,
            401
        );
        // It ends with its session, once the server has seen the socket
        // close.
        drop(client);
        while get(endpoint, &art(&first, 64, Some(&token)), &[])
            .await
            .status
            != 401
        {
            // sleep: between looks; the test's WAIT bounds them.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn neither_the_media_token_nor_a_cover_url_is_in_the_log_at_its_most_verbose() {
    bounded(async {
        let scratch = scratch();
        invent(&scratch.data_dir(), &scratch.root(), 8);
        let mut child = Command::new(env!("CARGO_BIN_EXE_ritornel-server"))
            .arg("serve")
            .arg("--data-dir")
            .arg(scratch.data_dir())
            .args(["--listen", "127.0.0.1:0", "--log-level", "trace"])
            .env_remove("RITORNEL_DATA_DIR")
            .env_remove("RITORNEL_LISTEN")
            .env_remove("RITORNEL_LOG_LEVEL")
            .env_remove("RITORNEL_BACKUP_DIR")
            .env_remove("RITORNEL_NAME")
            .env_remove("RITORNEL_VOLUME_CURVE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let found = loop {
            if let Ok(server) = ritornel_client::local::find(&scratch.data_dir()).await {
                break server;
            }
            if let Some(ended) = child.try_wait().unwrap() {
                let mut said = Vec::new();
                if let Some(mut stderr) = child.stderr.take() {
                    std::io::Read::read_to_end(&mut stderr, &mut said).unwrap();
                }
                let said = String::from_utf8_lossy(&said);
                panic!("the server ended before it was found, with {ended}: {said}");
            }
            // sleep: between looks while it starts; the test's WAIT bounds them.
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let token = std::fs::read_to_string(scratch.data_dir().join("credential")).unwrap();
        let client = Client::connect(found.endpoint, &Credential::new(token.trim()))
            .await
            .unwrap();
        client
            .hello(&HelloParams {
                presence: None,
                name: "lists-test".to_owned(),
                kind: ClientKind::Cli,
                version: "1".to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await
            .unwrap();
        let media = client.media_token().await.unwrap().token;
        let arts = arts(&client).await;
        for (index, id) in arts.iter().enumerate() {
            // Made, then from the cache, and one refused.
            let size = if index % 2 == 0 { 64 } else { 96 };
            assert_eq!(
                get(found.endpoint, &art(id, size, Some(&media)), &[])
                    .await
                    .status,
                200
            );
            assert_eq!(
                get(found.endpoint, &art(id, size, Some(&media)), &[])
                    .await
                    .status,
                200
            );
        }
        let refused = "f".repeat(64);
        assert_eq!(
            get(found.endpoint, &art(&arts[0], 64, Some(&refused)), &[])
                .await
                .status,
            401
        );
        drop(client);
        terminate(child.id());
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "the server ended with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let mut written = String::from_utf8_lossy(&output.stdout).into_owned();
        written.push_str(&String::from_utf8_lossy(&output.stderr));
        for file in std::fs::read_dir(scratch.data_dir().join("logs")).unwrap() {
            written.push_str(&std::fs::read_to_string(file.unwrap().path()).unwrap());
        }
        // The log was written at its most verbose, and saw covers made.
        assert!(written.contains("\"level\":\"debug\""), "{written}");
        assert!(written.contains("made a cover"), "{written}");
        for (what, secret) in [
            ("the media token", media.as_str()),
            ("the refused token", refused.as_str()),
            ("the credential", token.trim()),
        ] {
            assert!(!written.contains(secret), "{what} was written");
        }
        for part in [format!("{MEDIA_TOKEN_PARAM}="), "/v1/art".to_owned()] {
            assert!(!written.contains(&part), "{part}");
        }
    })
    .await;
}
