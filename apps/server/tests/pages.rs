//! Contract tests for an artist's page and an album's: `artist.get`,
//! `album.get`, and the lists the pages read through views (an artist's
//! discography and their songs on no record); and `history.track`, what
//! Lean back says of a song. A real server inside the
//! test, on an invented library with the hand-made showcase in it, talked
//! to through `ritornel-client`.
//!
//! Each test runs twice, joined to the dispatcher by channels in memory
//! and over a real socket (docs/design/contract.md, decision 4).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ritornel_client::contract::methods::{
    AlbumGet, ArtistGet, ArtistsImportPictures, ArtistsPictureFolders, HistoryTrack, LibrarySearch,
};
use ritornel_client::contract::{
    AlbumGetParams, AlbumRef, AlbumTile, ArtistGetParams, ArtistRow, CONTRACT_VERSION, ClientKind,
    ErrorCode, FILTER_VERSION, Filter, HelloParams, Id, ImportPicturesParams, ListKind,
    ListOpenParams, ListOpened, MAX_SEARCH_RESULTS, NoParams, PictureFolder, Predicate, Query,
    RecordType, Row, SearchParams, SearchResults, SessionKind, Shelf, SongRow, Sort, SortKey,
    TrackHistoryParams, Window,
};
use ritornel_client::{Client, Credential};
use ritornel_engine::{OsEntropy, SystemClock};
use ritornel_server::{Bounds, HeldDir, Listen, Server};

const WAIT: Duration = Duration::from_secs(60);

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
            "ritornel-server-pages-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn data_dir(&self) -> PathBuf {
        self.0.join("data")
    }

    fn root(&self) -> PathBuf {
        self.0.join("root")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy)]
enum Via {
    Memory,
    Socket,
}

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

/// Invents a library with the showcase in it, with the server's own hidden
/// command, as a developer would.
fn invent(data_dir: &Path, root: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_ritornel-server"))
        .arg("invent")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--root")
        .arg(root)
        .args(["--albums", "40", "--seed", "5", "--showcase"])
        .args(["--cover-min", "200", "--cover-max", "300"])
        .env_remove("RITORNEL_DATA_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Running {
    server: Server,
    scratch: Scratch,
}

impl Running {
    async fn start() -> Self {
        let scratch = Scratch::new();
        invent(&scratch.data_dir(), &scratch.root());
        let server = Server::start_with(
            HeldDir::take(scratch.data_dir()).unwrap(),
            Listen::on_port(0),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
            Bounds::default(),
        )
        .await
        .unwrap();
        Self { server, scratch }
    }

    async fn greeted(&self, via: Via) -> Client {
        let client = match via {
            Via::Memory => {
                let (outgoing, incoming) = self.server.connect_in_memory(SessionKind::Full);
                Client::over_memory(outgoing, incoming)
            }
            Via::Socket => {
                let token = std::fs::read_to_string(self.scratch.data_dir().join("credential"))
                    .unwrap()
                    .trim()
                    .to_owned();
                Client::connect(self.server.endpoint(), &Credential::new(token))
                    .await
                    .unwrap()
            }
        };
        client
            .hello(&HelloParams {
                name: "pages-test".to_owned(),
                kind: ClientKind::Cli,
                version: "1".to_owned(),
                contract_version: CONTRACT_VERSION,
            })
            .await
            .unwrap();
        client
    }
}

fn refused<T: std::fmt::Debug>(answer: Result<T, ritornel_client::Error>) -> ErrorCode {
    match answer {
        Err(ritornel_client::Error::Call(error)) => error.data.code,
        other => panic!("expected an error from the server, and got {other:?}"),
    }
}

fn filter(predicate: Predicate) -> Option<Filter> {
    Some(Filter {
        version: FILTER_VERSION,
        predicate,
    })
}

fn open(kind: ListKind, predicate: Predicate, sort: Option<SortKey>) -> ListOpenParams {
    ListOpenParams {
        kind,
        query: Query {
            filter: filter(predicate),
            sort: sort.map(|key| vec![Sort { key }]),
            ..Query::default()
        },
        window: Some(Window {
            offset: 0,
            limit: 500,
        }),
        replaces: None,
    }
}

/// The artist of this name.
async fn artist(client: &Client, name: &str) -> ArtistRow {
    let found = client
        .list_open(&ListOpenParams {
            kind: ListKind::Artists,
            query: Query {
                text: Some(name.to_owned()),
                ..Query::default()
            },
            window: Some(Window {
                offset: 0,
                limit: 50,
            }),
            replaces: None,
        })
        .await
        .unwrap();
    found
        .rows
        .into_iter()
        .find_map(|row| match row {
            Row::Artist(artist) if artist.name.as_deref() == Some(name) => Some(artist),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no artist named {name}"))
}

fn tiles(opened: &ListOpened) -> Vec<AlbumTile> {
    opened
        .rows
        .iter()
        .map(|row| match row {
            Row::Album(album) => album.clone(),
            other => panic!("{other:?}"),
        })
        .collect()
}

/// An artist's discography: its rows, and where each shelf begins.
struct Discography {
    opened: ListOpened,
    sections: Vec<(Shelf, usize)>,
}

impl Discography {
    fn total(&self) -> u64 {
        self.opened.total
    }

    fn tiles(&self) -> Vec<AlbumTile> {
        tiles(&self.opened)
    }

    /// The records of each shelf, in order.
    fn shelves(&self) -> Vec<(Shelf, Vec<AlbumTile>)> {
        let tiles = self.tiles();
        self.sections
            .iter()
            .enumerate()
            .map(|(at, (shelf, start))| {
                let end = self
                    .sections
                    .get(at + 1)
                    .map_or(tiles.len(), |(_, next)| *next);
                (*shelf, tiles[*start..end].to_vec())
            })
            .collect()
    }
}

/// Opens an artist's discography and reads its sections.
async fn discography(client: &Client, predicate: Predicate) -> Discography {
    let opened = client
        .list_open(&open(
            ListKind::Albums,
            predicate,
            Some(SortKey::Discography),
        ))
        .await
        .unwrap();
    let sections = client.list_sections(&opened.view).await.unwrap().sections;
    let sections = sections
        .iter()
        .map(|section| {
            let shelf = section
                .shelf
                .expect("a discography's section names its shelf");
            assert_eq!(section.label, shelf.label());
            (shelf, section.index as usize)
        })
        .collect();
    Discography { opened, sections }
}

fn title(tile: &AlbumTile) -> &str {
    tile.title.as_deref().unwrap_or_default()
}

/// Newest first by year, those with none last.
fn newest_first(tiles: &[AlbumTile]) -> bool {
    tiles
        .windows(2)
        .all(|pair| match (pair[0].year, pair[1].year) {
            (Some(a), Some(b)) => a >= b,
            (Some(_), None) | (None, None) => true,
            (None, Some(_)) => false,
        })
}

async fn a_discography_files_every_kind_newest_first_and_the_records_they_appear_on_last(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let calla = artist(&client, "Calla Brandt").await;
    let opened = discography(
        &client,
        Predicate::WithArtist {
            artist_id: calla.artist_id,
        },
    )
    .await;
    let shelves = opened.shelves();
    let order: Vec<Shelf> = shelves.iter().map(|(shelf, _)| *shelf).collect();
    assert_eq!(order, Shelf::ALL);
    for (shelf, tiles) in &shelves {
        assert!(newest_first(tiles), "{shelf:?}: {tiles:?}");
    }
    let titles = |shelf: Shelf| -> Vec<String> {
        shelves
            .iter()
            .find(|(each, _)| *each == shelf)
            .unwrap()
            .1
            .iter()
            .map(|tile| title(tile).to_owned())
            .collect()
    };
    // EPs are among the albums, and a record is placed by the year it first
    // came out: Low Country is 2017's, though a later edition is 2022's.
    assert_eq!(
        titles(Shelf::Albums),
        [
            "Weathervane",
            "Low Country",
            "Morning Static",
            "The Orchard Keeper",
            "Four Rooms",
            "Copper Sky"
        ]
    );
    assert_eq!(
        titles(Shelf::Singles),
        ["Static", "Halfway Bright", "Copper Sky"]
    );
    // A live EP is live.
    assert_eq!(
        titles(Shelf::Live),
        ["Four Rooms, Live", "Live at the Granary"]
    );
    assert_eq!(
        titles(Shelf::Collections),
        ["Weathervane, Remade", "B-Sides and Lost Rooms"]
    );
    // The records of others they are credited on a song of.
    assert_eq!(
        titles(Shelf::AppearsOn),
        [
            "Lights Out, Volume II",
            "Songs for the Harbour",
            "The Glass Orchard"
        ]
    );
    for tile in &shelves[4].1 {
        let credit = tile.artist_credit.as_ref().unwrap();
        assert!(!credit.artist_ids.contains(&calla.artist_id), "{tile:?}");
    }

    // Filtered by their credit alone, the same less the records of others.
    let theirs = discography(
        &client,
        Predicate::ByArtist {
            artist_id: calla.artist_id,
        },
    )
    .await;
    assert_eq!(theirs.total(), opened.total() - 3);
    assert_eq!(theirs.total(), calla.album_count);
    let order: Vec<Shelf> = theirs.shelves().iter().map(|(shelf, _)| *shelf).collect();
    assert_eq!(order, &Shelf::ALL[..4]);
}

async fn records_of_no_known_kind_are_albums_and_those_with_no_year_come_last(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let ensemble = artist(&client, "Søren Dunmore Ensemble").await;
    let opened = discography(
        &client,
        Predicate::WithArtist {
            artist_id: ensemble.artist_id,
        },
    )
    .await;
    let shelves = opened.shelves();
    assert_eq!(
        shelves.iter().map(|(shelf, _)| *shelf).collect::<Vec<_>>(),
        [Shelf::Albums, Shelf::AppearsOn]
    );
    let albums = &shelves[0].1;
    assert_eq!(albums.len(), 22);
    assert!(newest_first(albums));
    assert_eq!(title(&albums[0]), "Afterglow, Again");
    // The two with no year are last, by title.
    assert_eq!(
        albums[20..].iter().map(title).collect::<Vec<_>>(),
        ["Rooms Without Doors", "Tapes from the Boathouse"]
    );
    assert!(albums[20..].iter().all(|tile| tile.year.is_none()));
    // A record in versions is one tile, at its first year.
    let window = albums
        .iter()
        .find(|tile| title(tile) == "Northern Window")
        .unwrap();
    assert_eq!((window.year, window.version_count), (Some(1984), 3));
}

async fn songs_on_no_record_are_found_by_artist(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let calla = artist(&client, "Calla Brandt").await;
    let loose = client
        .list_open(&open(
            ListKind::Songs,
            Predicate::All {
                of: vec![
                    Predicate::ByArtist {
                        artist_id: calla.artist_id,
                    },
                    Predicate::NoRecord,
                ],
            },
            None,
        ))
        .await
        .unwrap();
    assert_eq!(loose.total, 3);
    for row in &loose.rows {
        let Row::Song(song) = row else { panic!() };
        assert_eq!(song.release_id, None);
        assert_eq!(song.album_title, None);
    }
    let all = client
        .list_open(&open(
            ListKind::Songs,
            Predicate::ByArtist {
                artist_id: calla.artist_id,
            },
            None,
        ))
        .await
        .unwrap();
    assert!(all.total > 100, "{}", all.total);
}

async fn a_list_that_cannot_be_asked_for_is_refused(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let calla = artist(&client, "Calla Brandt").await;
    let odile = artist(&client, "Odile Castellan").await;
    // A discography of nobody in particular.
    let mut nobody = open(
        ListKind::Albums,
        Predicate::NoRecord,
        Some(SortKey::Discography),
    );
    nobody.query.filter = None;
    assert_eq!(refused(client.list_open(&nobody).await), ErrorCode::Invalid);
    // Two artists at once.
    let two = open(
        ListKind::Albums,
        Predicate::All {
            of: vec![
                Predicate::ByArtist {
                    artist_id: calla.artist_id,
                },
                Predicate::WithArtist {
                    artist_id: odile.artist_id,
                },
            ],
        },
        None,
    );
    assert_eq!(refused(client.list_open(&two).await), ErrorCode::Invalid);
    // Records on no record, and songs filed as a discography.
    let records = open(ListKind::Albums, Predicate::NoRecord, None);
    assert_eq!(
        refused(client.list_open(&records).await),
        ErrorCode::Invalid
    );
    let songs = open(
        ListKind::Songs,
        Predicate::ByArtist {
            artist_id: calla.artist_id,
        },
        Some(SortKey::Discography),
    );
    assert_eq!(refused(client.list_open(&songs).await), ErrorCode::Invalid);
}

fn numbers(tracks: &[SongRow]) -> Vec<(i64, i64)> {
    tracks
        .iter()
        .map(|track| (track.disc.unwrap(), track.track.unwrap()))
        .collect()
}

async fn an_album_is_its_tracks_in_order_with_its_other_versions(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let ensemble = artist(&client, "Søren Dunmore Ensemble").await;
    let opened = discography(
        &client,
        Predicate::ByArtist {
            artist_id: ensemble.artist_id,
        },
    )
    .await;
    let records = opened.tiles();
    let tile = |name: &str| {
        records
            .iter()
            .find(|tile| title(tile) == name)
            .unwrap()
            .clone()
    };

    // A record in three versions shows the one its group is set to, the
    // first, and names the other two.
    let window = tile("Northern Window");
    let page = client
        .call::<AlbumGet>(&AlbumGetParams {
            album: window.album,
        })
        .await
        .unwrap();
    assert_eq!(page.tile.album, window.album);
    assert_eq!(page.release_group_id, window.album.id());
    assert_eq!(page.tile.title.as_deref(), Some("Northern Window"));
    assert_eq!((page.tile.year, page.tile.track_count), (Some(1984), 11));
    assert_eq!(page.tracks.len(), 11);
    assert_eq!(
        numbers(&page.tracks),
        (1..=11).map(|n| (1, n)).collect::<Vec<_>>()
    );
    assert_eq!(page.record_type, None);
    assert_eq!(page.shelf, Shelf::Albums);
    let versions: Vec<(String, Option<i64>, u64, bool)> = page
        .versions
        .iter()
        .map(|version| {
            (
                version.title.clone().unwrap(),
                version.year,
                version.track_count,
                version.shown,
            )
        })
        .collect();
    assert_eq!(
        versions,
        [
            (
                "Northern Window (Remastered)".to_owned(),
                Some(2009),
                11,
                false
            ),
            (
                "Northern Window (Deluxe Edition)".to_owned(),
                Some(2014),
                16,
                false
            ),
        ]
    );
    assert!(
        page.versions
            .iter()
            .all(|version| version.quality.is_some())
    );
    // One version by its own ID: its own year and tracks, and the first is
    // the one the group shows.
    let deluxe = page.versions[1].release_id;
    let page = client
        .call::<AlbumGet>(&AlbumGetParams {
            album: AlbumRef::Release { id: deluxe },
        })
        .await
        .unwrap();
    assert_eq!(page.release_id, deluxe);
    assert_eq!(page.release_group_id, window.album.id());
    assert_eq!(page.tile.album, AlbumRef::Release { id: deluxe });
    assert_eq!((page.tile.year, page.tracks.len()), (Some(2014), 16));
    assert_eq!(page.versions.len(), 2);
    assert!(page.versions[0].shown && !page.versions[1].shown);

    // Three discs, in disc and track order.
    let rooms = client
        .call::<AlbumGet>(&AlbumGetParams {
            album: tile("Collected Rooms").album,
        })
        .await
        .unwrap();
    let numbers = numbers(&rooms.tracks);
    assert_eq!(numbers.len(), 34);
    assert_eq!(numbers.first(), Some(&(1, 1)));
    assert_eq!(numbers.last(), Some(&(3, 10)));
    let mut sorted = numbers.clone();
    sorted.sort_unstable();
    assert_eq!(numbers, sorted);

    // A record of several artists names each song's own.
    let harbour = client
        .list_open(&ListOpenParams {
            kind: ListKind::Albums,
            query: Query {
                text: Some("Songs for the Harbour".to_owned()),
                ..Query::default()
            },
            window: Some(Window {
                offset: 0,
                limit: 5,
            }),
            replaces: None,
        })
        .await
        .unwrap();
    let harbour = tiles(&harbour)[0].clone();
    let page = client
        .call::<AlbumGet>(&AlbumGetParams {
            album: harbour.album,
        })
        .await
        .unwrap();
    assert_eq!(page.record_type, Some(RecordType::Album));
    assert_eq!(page.shelf, Shelf::Collections);
    let album_artists = &page.tile.artist_credit.as_ref().unwrap().artist_ids;
    assert!(page.tracks.iter().all(|track| {
        let credit = track.artist_credit.as_ref().unwrap();
        credit.artist_ids != *album_artists
    }));
}

async fn an_artist_is_their_row_with_the_colour_of_their_picture(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let calla = artist(&client, "Calla Brandt").await;
    let page = client
        .call::<ArtistGet>(&ArtistGetParams {
            artist_id: calla.artist_id,
        })
        .await
        .unwrap();
    assert_eq!(page.artist, calla);
    let art = page.artist.art_id.unwrap();
    let color = page.art_color.expect("a painted cover has a colour");
    assert!(color.hue < 360 && color.saturation <= 100);
    // Kept with the covers, and the same when asked again.
    let kept = running
        .scratch
        .data_dir()
        .join("cache/covers")
        .join(format!("{art}.color"));
    assert!(kept.exists(), "{}", kept.display());
    let again = client
        .call::<ArtistGet>(&ArtistGetParams {
            artist_id: calla.artist_id,
        })
        .await
        .unwrap();
    assert_eq!(again.art_color, Some(color));

    // An artist none of whose records has a cover has no colour either.
    let trio = artist(&client, "Bram Kowal Trio").await;
    let page = client
        .call::<ArtistGet>(&ArtistGetParams {
            artist_id: trio.artist_id,
        })
        .await
        .unwrap();
    assert_eq!((page.artist.art_id, page.art_color), (None, None));
    assert_eq!(page.artist.album_count, 2);

    // Names in other scripts are found and read back as written.
    for name in [
        "Мира Кесслер",
        "ナカムラ・ジュノ",
        "Θέα Λίντκβιστ",
        "카스텔란 사중주",
    ] {
        let found = artist(&client, name).await;
        assert!(found.album_count >= 1, "{name}");
    }
}

/// The first invented cover under `dir`.
fn a_cover(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = a_cover(&path) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|name| name == "cover.jpg") {
            return Some(path);
        }
    }
    None
}

async fn an_artist_takes_their_portrait_from_a_picture_folder(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let trio = artist(&client, "Bram Kowal Trio").await;
    assert_eq!((trio.art_id, trio.portrait), (None, false));
    let none = client
        .call::<ArtistsPictureFolders>(&NoParams {})
        .await
        .unwrap();
    assert!(none.folders.is_empty() && none.last_import_at.is_none());

    // A picture folder whose one portrait is an invented cover.
    let folder = running.scratch.0.join("artists");
    let dir = folder.join("by-name/Bram Kowal Trio");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(folder.join("index.json"), r#"{"format": 1, "artists": []}"#).unwrap();
    std::fs::copy(
        a_cover(&running.scratch.root()).unwrap(),
        dir.join("portrait.jpg"),
    )
    .unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"format": 1, "chosen": {"portrait": {"file": "portrait.jpg", "by": "owner"}}}"#,
    )
    .unwrap();
    let params = ImportPicturesParams {
        idempotency_key: Id::from_bytes([9; 16]),
        name: "artists".to_owned(),
        path: folder.to_str().unwrap().to_owned(),
    };
    let imported = client.call::<ArtistsImportPictures>(&params).await.unwrap();
    assert_eq!(
        (imported.by_name, imported.by_mbid, imported.dropped),
        (1, 0, 0)
    );
    assert_eq!(
        client.call::<ArtistsImportPictures>(&params).await.unwrap(),
        imported
    );
    // The folder is listed with where it is, and the import with its counts.
    let listed = client
        .call::<ArtistsPictureFolders>(&NoParams {})
        .await
        .unwrap();
    assert_eq!(
        listed.folders,
        [PictureFolder {
            name: "artists".to_owned(),
            path: Some(params.path.clone()),
            portraits: 1,
        }]
    );
    assert!(listed.last_import_at.is_some());
    assert_eq!(listed.last_import, Some(imported.clone()));

    // Their row and their page draw them with it, in its colour.
    let trio = artist(&client, "Bram Kowal Trio").await;
    assert!(trio.portrait && trio.art_id.is_some());
    let page = client
        .call::<ArtistGet>(&ArtistGetParams {
            artist_id: trio.artist_id,
        })
        .await
        .unwrap();
    assert_eq!(page.artist, trio);
    assert!(page.art_color.is_some());

    // A folder that is not a picture folder is refused.
    let elsewhere = ImportPicturesParams {
        idempotency_key: Id::from_bytes([10; 16]),
        name: "music".to_owned(),
        path: running.scratch.root().to_str().unwrap().to_owned(),
    };
    assert_eq!(
        refused(client.call::<ArtistsImportPictures>(&elsewhere).await),
        ErrorCode::Invalid
    );

    // The import sent again answers from its receipt, even with the folder
    // no longer a picture folder.
    std::fs::remove_file(folder.join("index.json")).unwrap();
    assert_eq!(
        client.call::<ArtistsImportPictures>(&params).await.unwrap(),
        imported
    );

    // A folder refused does not keep the name: it is free for the next.
    let wrong = running.scratch.0.join("wrong");
    std::fs::create_dir_all(&wrong).unwrap();
    std::fs::write(wrong.join("index.json"), r#"{"format": 2}"#).unwrap();
    let named = |key: u8, path: &Path| ImportPicturesParams {
        idempotency_key: Id::from_bytes([key; 16]),
        name: "faces".to_owned(),
        path: path.to_str().unwrap().to_owned(),
    };
    assert_eq!(
        refused(
            client
                .call::<ArtistsImportPictures>(&named(11, &wrong))
                .await
        ),
        ErrorCode::Invalid
    );
    std::fs::write(folder.join("index.json"), r#"{"format": 1, "artists": []}"#).unwrap();
    let elsewhere = client
        .call::<ArtistsImportPictures>(&named(12, &folder))
        .await
        .unwrap();
    assert_eq!(elsewhere.by_name, 1);
}

async fn what_the_library_does_not_have_is_not_found(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let nothing = Id::from_bytes([7; 16]);
    assert_eq!(
        refused(
            client
                .call::<ArtistGet>(&ArtistGetParams { artist_id: nothing })
                .await
        ),
        ErrorCode::NotFound
    );
    for album in [
        AlbumRef::ReleaseGroup { id: nothing },
        AlbumRef::Release { id: nothing },
    ] {
        assert_eq!(
            refused(client.call::<AlbumGet>(&AlbumGetParams { album }).await),
            ErrorCode::NotFound
        );
    }
    assert_eq!(
        refused(
            client
                .call::<HistoryTrack>(&TrackHistoryParams {
                    appearance_id: nothing
                })
                .await
        ),
        ErrorCode::NotFound
    );
}

/// Lean back's facts about a song nobody has played: none, so that the
/// window shows its title and artist alone.
async fn a_song_nobody_has_played_has_no_facts(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let ensemble = artist(&client, "Søren Dunmore Ensemble").await;
    let records = discography(
        &client,
        Predicate::ByArtist {
            artist_id: ensemble.artist_id,
        },
    )
    .await
    .tiles();
    let page = client
        .call::<AlbumGet>(&AlbumGetParams {
            album: records[0].album,
        })
        .await
        .unwrap();
    let history = client
        .call::<HistoryTrack>(&TrackHistoryParams {
            appearance_id: page.tracks[0].appearance_id,
        })
        .await
        .unwrap();
    assert!(history.facts.is_empty(), "{history:?}");
    assert_eq!(history.library_revision, page.library_revision);
}

async fn search(client: &Client, text: &str) -> SearchResults {
    client
        .call::<LibrarySearch>(&SearchParams {
            text: text.to_owned(),
            limit: None,
        })
        .await
        .unwrap()
}

async fn search_finds_names_as_they_are_typed_best_first(via: Via) {
    let running = Running::start().await;
    let client = running.greeted(via).await;
    let names = |found: &SearchResults| -> Vec<String> {
        found
            .artists
            .iter()
            .map(|artist| artist.name.clone().unwrap_or_default())
            .collect()
    };

    // Case and accents are ignored, and the artist whose name it is comes
    // first.
    for typed in ["SØREN", "soren", "søren dunmore ensemble"] {
        let found = search(&client, typed).await;
        assert_eq!(found.top, Some(ListKind::Artists), "{typed}");
        assert_eq!(
            names(&found).first().map(String::as_str),
            Some("Søren Dunmore Ensemble"),
            "{typed}"
        );
    }
    // By the name they are filed under, and in another script.
    let found = search(&client, "castellan").await;
    assert_eq!(
        names(&found).first().map(String::as_str),
        Some("Odile Castellan")
    );
    let found = search(&client, "мира").await;
    assert_eq!(names(&found), ["Мира Кесслер"]);

    // A record by its title, shown once however many versions it has, and
    // a song by its title.
    let found = search(&client, "northern window").await;
    assert_eq!(found.top, Some(ListKind::Albums));
    let titles: Vec<_> = found.albums.iter().map(|tile| tile.title.clone()).collect();
    assert_eq!(titles, [Some("Northern Window".to_owned())]);
    let page = client
        .call::<AlbumGet>(&AlbumGetParams {
            album: found.albums[0].album,
        })
        .await
        .unwrap();
    let track = page.tracks[0].clone();
    let found = search(&client, track.title.as_deref().unwrap()).await;
    assert!(
        found
            .songs
            .iter()
            .any(|found| found.song.recording_id == track.recording_id),
        "{:?}",
        track.title
    );

    // Each kind at most as many as asked, and nothing for nothing typed.
    let found = client
        .call::<LibrarySearch>(&SearchParams {
            text: "a".to_owned(),
            limit: Some(2),
        })
        .await
        .unwrap();
    assert!(found.artists.len() <= 2 && found.albums.len() <= 2 && found.songs.len() <= 2);
    assert!(!found.albums.is_empty());
    let found = search(&client, "  ").await;
    assert!(found.artists.is_empty() && found.albums.is_empty() && found.songs.is_empty());
    assert_eq!(
        refused(
            client
                .call::<LibrarySearch>(&SearchParams {
                    text: "a".to_owned(),
                    limit: Some(MAX_SEARCH_RESULTS + 1),
                })
                .await
        ),
        ErrorCode::Invalid
    );
}

on_both_transports! {
    search_finds_names_as_they_are_typed_best_first
    a_discography_files_every_kind_newest_first_and_the_records_they_appear_on_last
    records_of_no_known_kind_are_albums_and_those_with_no_year_come_last
    songs_on_no_record_are_found_by_artist
    a_list_that_cannot_be_asked_for_is_refused
    an_album_is_its_tracks_in_order_with_its_other_versions
    an_artist_is_their_row_with_the_colour_of_their_picture
    an_artist_takes_their_portrait_from_a_picture_folder
    what_the_library_does_not_have_is_not_found
    a_song_nobody_has_played_has_no_facts
}
