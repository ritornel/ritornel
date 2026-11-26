//! An artist's page: the banner from edge to edge made of their picture
//! (`banner.rs`), with their portrait standing clear at the right (or, with
//! none, the cover their picture is), and under it everything of theirs,
//! filed as a discography the way a person who knows the records would:
//! albums first, then singles, live recordings, collections and rarities,
//! the records they appear on, and last their songs that are on no record.
//! The server files them; the page lays them out. Each kind takes only the
//! room it needs, so a kind with two records sits beside the albums rather
//! than alone in a wide row.
//!
//! Your history with them (the years, and the records you played most)
//! stands in a column beside the records when there is room for it, and
//! stays as they scroll; otherwise it goes under them. With no plays of
//! theirs there is none.
//!
//! What it reads: `artist.get`, their records and their songs on no record
//! as whole lists read through views closed at once, and `history.artist`;
//! again when the library changes, and after a change of plays alone only
//! their songs and your history with them.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ObjectFit, ParentElement, Render, RenderImage, ScrollEvent, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, Task, Window, canvas, div, img,
    prelude::FluentBuilder, px,
};
use ritornel_client::Client;
use ritornel_contract::methods::{ArtistGet, HistoryArtist};
use ritornel_contract::{
    AlbumTile, ArtistGetParams, ArtistPage, ContextKind, Event, FILTER_VERSION, Filter,
    HistoryArtistParams, HistoryOf, Id, LibraryChange, ListKind, ListOpenParams, PlayContext,
    Predicate, Query, RecordPlayed, Row, Section, Shelf, SongRow, Sort, SortKey,
};

use crate::banner::{self, Actions, Banner};
use crate::link::{Link, LinkEvent};
use crate::net::{Net, NoArt};
use crate::player::{Doing, Many, Player};
use crate::record::{self, Words};
use crate::theme;
use crate::tracks::Tracks;
use crate::wall::{TileRow, viewport_of};
use crate::words::artist::{
    CARD, FIRST_CARD, HISTORY_ACROSS, HISTORY_GAP, SHELF_GAP, artist_facts, has_history,
    history_beside, history_column, shelf_heading, song_columns,
};
use crate::words::{pictured, plural, said};

/// The page's measure: it centres once the window is wider than this.
const MEASURE: f32 = 2000.;
/// The most rows one window of a list may hold: the server's limit.
const WINDOW: u64 = 500;
/// Where your history with them stays, under the banner's strip.
const STAYS_AT: f32 = 76.;
/// Between the parts of the page.
const PART_GAP: f32 = 44.;
/// Byte budget for shelf covers kept on one artist page.
const MAX_COVER_BYTES: usize = 16 * 1024 * 1024;

/// What the page asks of the window.
pub(crate) enum ArtistEvent {
    /// Open a record's page, its cover lifted from here when it is shown.
    Open(Arc<TileRow>, Option<record::Lift>),
    /// Up to all the artists, at this one.
    Up(Id),
}

/// A list read whole: its rows and its sections.
struct Whole {
    rows: Vec<Row>,
    sections: Vec<Section>,
    revision: u64,
}

/// Reads a whole list through a view of its own, and closes the view.
async fn read_whole(
    client: &Client,
    kind: ListKind,
    query: Query,
) -> Result<Whole, ritornel_client::Error> {
    let opened = client
        .list_open(&ListOpenParams {
            kind,
            query,
            window: Some(ritornel_contract::Window {
                offset: 0,
                limit: WINDOW,
            }),
            replaces: None,
        })
        .await?;
    let mut rows = opened.rows;
    let read = async {
        while (rows.len() as u64) < opened.total {
            let window = client
                .list_window(&opened.view, rows.len() as u64, WINDOW)
                .await?;
            if window.rows.is_empty() {
                break;
            }
            rows.extend(window.rows);
        }
        client.list_sections(&opened.view).await
    };
    let sections = read.await;
    let _ = client.list_close(&opened.view).await;
    Ok(Whole {
        rows,
        sections: sections?.sections,
        revision: opened.library_revision,
    })
}

fn filter(predicate: Predicate) -> Option<Filter> {
    Some(Filter {
        version: FILTER_VERSION,
        predicate,
    })
}

/// Rows into shelves, by the sections that say where each begins.
fn shelve(rows: &[Row], sections: &[Section]) -> Vec<(Shelf, Vec<AlbumTile>)> {
    let mut shelves = Vec::new();
    for (at, section) in sections.iter().enumerate() {
        let start = section.index as usize;
        let end = sections
            .get(at + 1)
            .map_or(rows.len(), |next| next.index as usize);
        let records: Vec<AlbumTile> = rows
            .get(start..end.min(rows.len()))
            .unwrap_or_default()
            .iter()
            .filter_map(|row| match row {
                Row::Album(tile) => Some(tile.clone()),
                _ => None,
            })
            .collect();
        if let Some(shelf) = section.shelf
            && !records.is_empty()
        {
            shelves.push((shelf, records));
        }
    }
    shelves
}

fn songs_of(whole: Whole) -> Vec<SongRow> {
    whole
        .rows
        .into_iter()
        .filter_map(|row| match row {
            Row::Song(song) => Some(song),
            _ => None,
        })
        .collect()
}

/// Everything the page reads, at once.
struct Read {
    artist: Option<ArtistPage>,
    shelves: Option<Vec<(Shelf, Vec<AlbumTile>)>>,
    songs: Vec<SongRow>,
    songs_revision: u64,
    history: Option<HistoryOf>,
    revision: u64,
}

/// The page's pictures whose last read failed this time, as reads do
/// while a satellite's hub is away: not those the server has none of or
/// cannot make. They are kept for the picture the page shows, and let go
/// with it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Unread {
    face: bool,
    portrait: bool,
    /// The banner's blur and the wash's, read together.
    blurs: bool,
}

/// One of the page's pictures, as `Unread` keeps them.
#[derive(Debug, Clone, Copy)]
enum Picture {
    Face,
    Portrait,
    Blurs,
}

impl Unread {
    fn of(&mut self, picture: Picture) -> &mut bool {
        match picture {
            Picture::Face => &mut self.face,
            Picture::Portrait => &mut self.portrait,
            Picture::Blurs => &mut self.blurs,
        }
    }
}

pub(crate) struct Artist {
    net: Arc<Net>,
    player: Entity<Player>,
    pub artist_id: Id,
    name: SharedString,
    /// Where its records have their covers, for them to travel from and back to.
    pub landings: Rc<record::Landings>,
    /// The thing whose menu is open, or closing, as the page began to draw.
    menued: Option<(String, f32)>,
    page: Option<ArtistPage>,
    shelves: Vec<(Shelf, Vec<AlbumTile>)>,
    tracks: Entity<Tracks>,
    history: Option<HistoryOf>,
    covers: HashMap<(Id, u32), (Arc<RenderImage>, usize, u64)>,
    cover_bytes: usize,
    covers_scale: f32,
    frame: u64,
    asked: HashSet<(Id, u32)>,
    retries: crate::covers::Retries,
    fetches: crate::covers::FetchBag,
    /// Generation bumped whenever in-flight fetches are aborted so late
    /// cancellation callbacks cannot mark cancelled work as failed.
    fetch_gen: u64,
    /// Which of the artist's header pictures have an active read in flight.
    in_flight_pictures: Unread,
    revision: u64,
    /// Whether it is behind another page, kept for Back, and what changed
    /// meanwhile.
    freshness: crate::stage::Freshness,
    /// Whether the read wanted is of the whole page: a read of plays alone
    /// does not stand in for it.
    whole_wanted: bool,
    reading: Option<Task<()>>,
    problem: bool,
    face: Option<Arc<RenderImage>>,
    /// Their portrait as it came, at its own shape.
    portrait: Option<Arc<image::RgbaImage>>,
    /// The portrait made for the banner's box (`banner::melted`): the box's
    /// size in device pixels, the picture, and when the first was shown.
    melted: Option<((u32, u32), Arc<RenderImage>, Instant)>,
    /// Whether the portrait is away while a copy of it travels.
    pub portrait_away: bool,
    /// Whether the banner's picture could not be read: then there is none
    /// to wait for, or to land on.
    picture_failed: bool,
    /// Whether a portrait is being made now: one at a time.
    melting: bool,
    /// The size its box has now, in device pixels: what is made next.
    wanted: Option<(u32, u32)>,
    /// Their picture blurred for the wash, and for the banner.
    pub blur: crate::blur::Shown,
    banner_blur: crate::blur::Shown,
    /// The picture the page's pictures were made from, and whether it is
    /// their portrait: a portrait added or taken away can keep the picture's
    /// ID, when it is the same as a cover's.
    pictured_for: Option<(Option<Id>, bool)>,
    /// The display's scale they were read for: on another display the face
    /// and the portrait are read again, sharp there.
    pictured_scale: f32,
    /// The last read of the face, the portrait and the blurs asked for: an earlier
    /// one coming late, for another display or picture, is let go.
    face_read: u64,
    portrait_read: u64,
    blur_read: u64,
    /// The pictures that failed this time, and those to ask for again at
    /// the next draw, once each: once a session begins, or the hub is back.
    unread: Unread,
    read_unread: Unread,
    /// Counts the sessions begun and the hub's returns: a read begun before
    /// the last that fails after it is asked for again.
    returns: u64,
    /// Whether the face and the blurs shown are another picture's, or
    /// none: then one that cannot be read gives soft colour in its place,
    /// and otherwise what is shown stays.
    stale_face: bool,
    stale_blurs: bool,
    /// How many reads of pictures are under way, for tests to wait on.
    #[cfg(test)]
    pictures_in_flight: usize,
    pub top: f32,
    elastic: crate::elastic::Elastic,
    /// How tall the page was when last drawn, its top marker in window
    /// coordinates (for scroll-invariant card bounds), the width those bounds
    /// were measured at, and the column of history and what it stands beside.
    head: Rc<Cell<f32>>,
    measured_area: f32,
    height: Rc<Cell<f32>>,
    column: Rc<Cell<f32>>,
    beside_height: Rc<Cell<f32>>,
    /// How it shows while it arrives or leaves, as the stage moves it.
    pub look: crate::moves::Look,
    _link: Subscription,
}

/// How far the banner's blur and the wash's spread, for the window's size.
fn blur_radii(window: &Window) -> (usize, usize) {
    let (width, viewport) = viewport_of(window);
    (
        crate::blur::radius(crate::blur::BANNER.0, width + 120.),
        crate::blur::radius(crate::blur::WASH.0, width.max(viewport + 128.) + 180.),
    )
}

impl EventEmitter<ArtistEvent> for Artist {}

impl Artist {
    pub(crate) fn new(
        net: Arc<Net>,
        link: &Entity<Link>,
        player: Entity<Player>,
        artist_id: Id,
        name: SharedString,
        cx: &mut Context<Self>,
    ) -> Self {
        // When the library changes, what the page shows is read again: through
        // a new session when the server is found again, and when a
        // satellite's hub is back.
        let followed = cx.subscribe(link, |page: &mut Self, _, event: &LinkEvent, cx| {
            let plays_only = match event {
                LinkEvent::Connected(net) => {
                    page.net = Arc::clone(net);
                    // Revisions are the server's: another may count from
                    // lower.
                    page.revision = 0;
                    // What could not be read is asked for again at once.
                    page.retries = crate::covers::Retries::default();
                    page.returns += 1;
                    page.read_unread = page.unread;
                    false
                }
                // The same session, its hub back: what could not be read
                // while it was away is read again, once.
                LinkEvent::Restated(net) => {
                    let back = crate::away::back(&page.net, net);
                    page.net = Arc::clone(net);
                    if !back {
                        return;
                    }
                    page.retries = crate::covers::Retries::default();
                    page.returns += 1;
                    page.read_unread = page.unread;
                    false
                }
                LinkEvent::Server(Event::Library(event))
                    if event.library_revision > page.revision =>
                {
                    !event.changed.is_empty()
                        && event
                            .changed
                            .iter()
                            .all(|change| *change == LibraryChange::Stats)
                }
                _ => return,
            };
            if let Some(plays_only) = page.freshness.heard(plays_only) {
                page.read(plays_only, cx);
            }
        });
        let mut page = Self {
            net,
            player: player.clone(),
            artist_id,
            name,
            landings: Rc::default(),
            menued: None,
            page: None,
            shelves: Vec::new(),
            tracks: cx.new(|cx| Tracks::new(player, false, cx)),
            history: None,
            covers: HashMap::new(),
            cover_bytes: 0,
            covers_scale: 0.,
            frame: 0,
            asked: HashSet::new(),
            retries: crate::covers::Retries::default(),
            fetches: crate::covers::FetchBag::default(),
            fetch_gen: 0,
            in_flight_pictures: Unread::default(),
            revision: 0,
            freshness: crate::stage::Freshness::default(),
            whole_wanted: false,
            reading: None,
            problem: false,
            face: None,
            portrait: None,
            melted: None,
            melting: false,
            portrait_away: false,
            picture_failed: false,
            wanted: None,
            blur: None,
            banner_blur: None,
            pictured_for: None,
            pictured_scale: 0.,
            face_read: 0,
            portrait_read: 0,
            blur_read: 0,
            unread: Unread::default(),
            read_unread: Unread::default(),
            returns: 0,
            stale_face: true,
            stale_blurs: true,
            #[cfg(test)]
            pictures_in_flight: 0,
            top: 0.,
            elastic: crate::elastic::Elastic::default(),
            head: Rc::new(Cell::new(f32::NAN)),
            measured_area: 0.,
            height: Rc::new(Cell::new(0.)),
            column: Rc::new(Cell::new(0.)),
            beside_height: Rc::new(Cell::new(0.)),
            look: crate::moves::Look::default(),
            _link: followed,
        };
        cx.on_release(|page, cx| {
            page.release_images(cx);
        })
        .detach();
        page.read(false, cx);
        page
    }

    /// Drops every picture this page owns from the sprite atlas when the page
    /// is released.
    fn release_images(&mut self, cx: &mut gpui::App) {
        let mut released = false;
        let mut drop_one = |image: Arc<RenderImage>| {
            released = true;
            crate::covers::drop_unshared(image, None, cx);
        };
        self.cover_bytes = 0;
        for (_, (image, _, _)) in self.covers.drain() {
            drop_one(image);
        }
        if let Some(image) = self.face.take() {
            drop_one(image);
        }
        if let Some((_, image, _)) = self.melted.take() {
            drop_one(image);
        }
        if let Some((image, _)) = self.blur.take() {
            drop_one(image);
        }
        if let Some((image, _)) = self.banner_blur.take() {
            drop_one(image);
        }
        if released {
            crate::platform::give_back();
        }
    }

    /// Releases cached pictures when this page sits deep in back history,
    /// resetting picture state so they reload if the user navigates back.
    pub(crate) fn trim_behind(&mut self, cx: &mut Context<Self>) {
        self.fetch_gen = self.fetch_gen.wrapping_add(1);
        self.fetches.clear();
        self.in_flight_pictures = Unread::default();
        self.release_images(cx);
        self.asked.clear();
        self.portrait = None;
        self.melting = false;
        self.pictured_for = None;
        self.pictured_scale = 0.;
        self.stale_face = true;
        self.stale_blurs = true;
        self.picture_failed = false;
    }

    /// The page shown again, or put behind another: what changed while it
    /// was behind is read.
    pub(crate) fn seen(&mut self, seen: bool, cx: &mut Context<Self>) {
        if !seen {
            self.fetch_gen = self.fetch_gen.wrapping_add(1);
            self.fetches.clear();
            self.asked.retain(|key| self.covers.contains_key(key));
            self.melting = false;
            let cancelled = std::mem::take(&mut self.in_flight_pictures);
            self.read_unread.face |= cancelled.face;
            self.read_unread.portrait |= cancelled.portrait;
            self.read_unread.blurs |= cancelled.blurs;
        }
        if let Some(plays_only) = self.freshness.shown(seen) {
            self.read(plays_only, cx);
        }
    }

    /// Reads the page, or after a change of plays alone only their songs
    /// and your history with them.
    fn read(&mut self, plays_only: bool, cx: &mut Context<Self>) {
        let plays_only = plays_only && !self.whole_wanted;
        self.whole_wanted = !plays_only;
        let client = Arc::clone(&self.net.client);
        let artist_id = self.artist_id;
        let read = self.net.rt().spawn(async move {
            let params = ArtistGetParams { artist_id };
            let loose = read_whole(
                &client,
                ListKind::Songs,
                Query {
                    filter: filter(Predicate::All {
                        of: vec![Predicate::ByArtist { artist_id }, Predicate::NoRecord],
                    }),
                    ..Query::default()
                },
            );
            let of = HistoryArtistParams {
                artist_id: Some(artist_id),
                key: None,
            };
            let history = client.call::<HistoryArtist>(&of);
            if plays_only {
                let (loose, history) = tokio::join!(loose, history);
                let loose = loose?;
                return Ok::<_, ritornel_client::Error>(Read {
                    artist: None,
                    shelves: None,
                    revision: loose.revision,
                    songs_revision: loose.revision,
                    songs: songs_of(loose),
                    history: history.ok(),
                });
            }
            let records = read_whole(
                &client,
                ListKind::Albums,
                Query {
                    filter: filter(Predicate::WithArtist { artist_id }),
                    sort: Some(vec![Sort {
                        key: SortKey::Discography,
                    }]),
                    ..Query::default()
                },
            );
            let (artist, records, loose, history) =
                tokio::join!(client.call::<ArtistGet>(&params), records, loose, history);
            let (artist, records, loose) = (artist?, records?, loose?);
            Ok(Read {
                revision: artist
                    .library_revision
                    .max(records.revision)
                    .max(loose.revision),
                artist: Some(artist),
                shelves: Some(shelve(&records.rows, &records.sections)),
                songs_revision: loose.revision,
                songs: songs_of(loose),
                history: history.ok(),
            })
        });
        self.reading = Some(cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |page, cx| {
                match answer {
                    Ok(Ok(read)) => {
                        // The records are what a whole read adds: until
                        // they come, one is still wanted.
                        if read.shelves.is_some() {
                            page.whole_wanted = false;
                        }
                        page.took(read, cx);
                    }
                    _ => page.problem = page.page.is_none(),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn took(&mut self, read: Read, cx: &mut Context<Self>) {
        self.revision = self.revision.max(read.revision).max(
            read.history
                .as_ref()
                .map_or(0, |history| history.library_revision),
        );
        if let Some(history) = read.history {
            self.history = Some(history);
        }
        if let Some(artist) = read.artist {
            if let Some(name) = &artist.artist.name {
                self.name = name.clone().into();
            }
            self.page = Some(artist);
        }
        if let Some(shelves) = read.shelves {
            self.shelves = shelves;
            self.measured_area = 0.;
        }
        let context = PlayContext {
            kind: ContextKind::Artist,
            reference: Some(self.artist_id),
        };
        self.tracks.update(cx, |tracks, cx| {
            tracks.set(read.songs, read.songs_revision, None, context, cx)
        });
        self.problem = false;
    }

    /// Reads a cover at `px` pixels, once, and keeps it. One that could not
    /// be read is asked for again after a while.
    fn want(&mut self, art: Id, px: u32, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let image = self.cover_at(art, px, cx);
        let exact = self.covers.contains_key(&(art, px));
        crate::waits::drawn("artist", art, px, exact);
        image
    }

    fn store_cover(
        &mut self,
        art: Id,
        px: u32,
        image: Arc<RenderImage>,
        bytes: usize,
        cx: &mut gpui::App,
    ) {
        // Evict any stale-DPI variant of the same cover once a new-scale size lands,
        // keeping valid sibling slot sizes (30/140/220 pt) at the current display scale.
        let scale = self.covers_scale;
        let current_slot = |size: u32| {
            scale > 0.
                && [30., 140., 220.]
                    .into_iter()
                    .any(|pt| (pt * scale).ceil() as u32 == size)
        };
        let stale: Vec<(Id, u32)> = self
            .covers
            .keys()
            .copied()
            .filter(|&(id, size)| id == art && size != px && !current_slot(size))
            .collect();
        for key in stale {
            if let Some((old, old_bytes, _)) = self.covers.remove(&key) {
                self.cover_bytes = self.cover_bytes.saturating_sub(old_bytes);
                self.asked.remove(&key);
                crate::covers::drop_unshared(old, None, cx);
            }
        }
        if let Some((old, old_bytes, _)) = self.covers.insert((art, px), (image, bytes, self.frame))
        {
            self.cover_bytes = self.cover_bytes.saturating_sub(old_bytes);
            crate::covers::drop_unshared(old, None, cx);
        }
        self.cover_bytes += bytes;
        if self.cover_bytes <= MAX_COVER_BYTES {
            return;
        }
        let mut old: Vec<(u64, (Id, u32))> = self
            .covers
            .iter()
            .filter(|(_, (_, _, used))| used.saturating_add(1) < self.frame)
            .map(|(key, (_, _, used))| (*used, *key))
            .collect();
        old.sort_unstable_by_key(|(used, _)| *used);
        for (_, key) in old {
            if self.cover_bytes <= MAX_COVER_BYTES {
                break;
            }
            if let Some((gone, gone_bytes, _)) = self.covers.remove(&key) {
                self.cover_bytes = self.cover_bytes.saturating_sub(gone_bytes);
                self.asked.remove(&key);
                crate::covers::drop_unshared(gone, None, cx);
            }
        }
    }

    fn cover_at(&mut self, art: Id, px: u32, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        if let Some((image, _, used)) = self.covers.get_mut(&(art, px)) {
            *used = self.frame;
            return Some(Arc::clone(image));
        }
        if !self.retries.waits(art, Instant::now()) && self.asked.insert((art, px)) {
            self.fetch_sized(art, px, cx, move |page, image, cx| {
                let Some(image) = image else {
                    return;
                };
                match image {
                    Ok((image, bytes)) => {
                        page.retries.read(art);
                        page.store_cover(art, px, image, bytes, cx);
                    }
                    // Not there: it is not asked for again.
                    Err(NoArt::Missing) => {}
                    Err(NoArt::Failed) => {
                        page.asked.remove(&(art, px));
                        // Shown at two sizes, it fails twice: the wait grows once.
                        let now = Instant::now();
                        if !page.retries.waits(art, now) {
                            let wait = page.retries.failed(art, now);
                            crate::covers::notify_after(wait, cx);
                        }
                    }
                }
            });
        }
        // Meanwhile, as it was read for another size or display.
        self.covers
            .iter()
            .filter(|((at, _), _)| *at == art)
            .max_by_key(|((_, size), _)| *size)
            .map(|(_, (image, _, _))| Arc::clone(image))
    }

    fn fetch_sized(
        &mut self,
        art: Id,
        px: u32,
        cx: &mut Context<Self>,
        took: impl FnOnce(
            &mut Self,
            Option<Result<(Arc<RenderImage>, usize), NoArt>>,
            &mut Context<Self>,
        ) + 'static,
    ) {
        let generation = self.fetch_gen;
        let http = Arc::clone(&self.net.art);
        let sizes = self.net.art_sizes.clone();
        let read = self
            .net
            .rt()
            .spawn(async move { crate::covers::fetch_square(&http, &sizes, art, px).await });
        self.fetches.push(read.abort_handle());
        cx.spawn(async move |this, cx| {
            let image = read.await.ok();
            this.update(cx, |page, cx| {
                let result = if page.fetch_gen == generation {
                    image.map(|res| res.map(|(image, _, bytes)| (Arc::new(image), bytes)))
                } else {
                    None
                };
                took(page, result, cx);
                if page.fetch_gen == generation {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn fetch(
        &mut self,
        art: Id,
        px: u32,
        cx: &mut Context<Self>,
        took: impl FnOnce(&mut Self, Option<Result<Arc<RenderImage>, NoArt>>, &mut Context<Self>)
        + 'static,
    ) {
        self.fetch_sized(art, px, cx, move |page, result, cx| {
            took(page, result.map(|res| res.map(|(image, _)| image)), cx);
        });
    }

    /// Makes the portrait for its box, `size` device pixels, off the UI
    /// thread, one at a time. One made for a size the box no longer has, or
    /// from a portrait since replaced, is let go and the next is made for the
    /// size the box has then; so is the one a new one replaces, so a window
    /// resized narrow does not leave one behind at every width.
    fn melt(&mut self, size: (u32, u32), cx: &mut Context<Self>) {
        self.wanted = Some(size);
        let Some(source) = &self.portrait else { return };
        if self.melting || self.melted.as_ref().is_some_and(|made| made.0 == size) {
            return;
        }
        self.melting = true;
        let generation = self.fetch_gen;
        let source = Arc::clone(source);
        let from = Arc::clone(&source);
        let made = self.net.rt().spawn(crate::covers::decode(move || {
            Some(crate::banner::melted(&source, size.0, size.1))
        }));
        self.fetches.push(made.abort_handle());
        cx.spawn(async move |this, cx| {
            let made = made.await.ok().flatten();
            this.update(cx, |page, cx| {
                if page.fetch_gen != generation {
                    return;
                }
                page.melting = false;
                let current = page
                    .portrait
                    .as_ref()
                    .is_some_and(|portrait| Arc::ptr_eq(portrait, &from));
                if let Some(image) = made
                    && current
                    && page.wanted == Some(size)
                {
                    // Made while a copy of it travels, it is shown whole when
                    // it lands, not faded in again.
                    let since = page.melted.as_ref().map_or_else(
                        || {
                            let now = Instant::now();
                            if page.portrait_away {
                                now.checked_sub(std::time::Duration::from_millis(300))
                                    .unwrap_or(now)
                            } else {
                                now
                            }
                        },
                        |made| made.2,
                    );
                    if let Some((_, old, _)) = page.melted.replace((size, Arc::new(image), since)) {
                        crate::covers::drop_unshared(old, None, cx);
                    }
                }
                // The next render makes the one wanted now, if this was not it.
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Lets the portrait go, and the one made from it. While the window is
    /// drawing, GPUI holds it apart from its others, and lets an image go
    /// from it only when it is handed the window.
    fn forget_portrait(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.portrait = None;
        if let Some((_, old, _)) = self.melted.take() {
            crate::covers::drop_unshared(old, window, cx);
        }
    }

    /// Their picture: the face for the strip, the portrait, and the blurs
    /// for the banner and the wash; with no picture, soft colour from their name.
    fn pictures(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(artist) = &self.page else { return };
        let (art, wants_portrait) = (artist.artist.art_id, artist.artist.portrait);
        let scale = window.scale_factor();
        if self.pictured_for == Some((art, wants_portrait)) {
            // On another display, read sharp there first, so that what is
            // asked for again below is asked for at its scale, and once.
            let sharpened = self.pictured_scale != scale;
            let portrait_read = sharpened && self.sharpen(art, scale, window, cx);
            // Those that failed this time, and only those, are asked for
            // again, once.
            let again = std::mem::take(&mut self.read_unread);
            if let Some(art) = art {
                if again.face && !sharpened {
                    self.read_face(art, scale, cx);
                }
                if again.portrait && !portrait_read {
                    self.read_portrait(art, scale, cx);
                }
                if again.blurs {
                    self.read_blurs(art, window, cx);
                }
            }
            return;
        }
        self.pictured_for = Some((art, wants_portrait));
        self.pictured_scale = scale;
        self.unread = Unread::default();
        self.read_unread = Unread::default();
        self.in_flight_pictures = Unread::default();
        self.stale_face = true;
        self.stale_blurs = true;
        // A portrait of another picture, or of none, is not kept meanwhile.
        self.forget_portrait(Some(window), cx);
        self.picture_failed = false;
        let Some(art) = art else {
            self.blur_read += 1;
            let (banner_radius, wash_radius) = blur_radii(window);
            self.soft_pictures(banner_radius, wash_radius, Some(window), cx);
            return;
        };
        self.read_face(art, scale, cx);
        self.read_portrait(art, scale, cx);
        self.read_blurs(art, window, cx);
    }

    /// A read of `picture` came back, `failed` this time or not, having
    /// begun when `returns` counted `since`. One that failed is asked for
    /// again: at the next draw if a session began or the hub came back
    /// since it began, else when that next happens.
    fn came_back(&mut self, picture: Picture, failed: bool, since: u64) {
        *self.unread.of(picture) = failed;
        if failed && since != self.returns {
            *self.read_unread.of(picture) = true;
        }
    }

    /// The blurs of their picture for the banner and the wash, read
    /// together. Where another picture's are shown, or none, one the server
    /// cannot make, or that could not be read for now, gives soft colour
    /// from their name instead; otherwise what is shown stays.
    fn read_blurs(&mut self, art: Id, window: &Window, cx: &mut Context<Self>) {
        self.blur_read += 1;
        let blur_read = self.blur_read;
        let generation = self.fetch_gen;
        let since = self.returns;
        self.in_flight_pictures.blurs = true;
        #[cfg(test)]
        {
            self.pictures_in_flight += 1;
        }
        let (banner_radius, wash_radius) = blur_radii(window);
        let http = Arc::clone(&self.net.art);
        let tone = crate::blur::wash_tone(theme::dark());
        let blurs = self.net.rt().spawn(async move {
            let bytes = http.get(art, crate::blur::SIDE).await?;
            crate::covers::decode(move || {
                Some((
                    crate::blur::jpeg(&bytes, banner_radius, crate::blur::BANNER.1)?,
                    crate::blur::jpeg_toned(&bytes, wash_radius, tone.0, tone.1)?,
                ))
            })
            .await
            .ok_or(NoArt::Missing)
        });
        self.fetches.push(blurs.abort_handle());
        cx.spawn(async move |this, cx| {
            let blurs = blurs.await.ok();
            this.update(cx, |page, cx| {
                #[cfg(test)]
                {
                    page.pictures_in_flight -= 1;
                }
                let Some(blurs) = blurs else {
                    return;
                };
                // Too late: the page left or asked for its pictures again since.
                if page.fetch_gen != generation || page.blur_read != blur_read {
                    return;
                }
                page.in_flight_pictures.blurs = false;
                page.came_back(Picture::Blurs, blurs == Err(NoArt::Failed), since);
                match blurs {
                    Ok((banner, wash)) => {
                        let now = Instant::now();
                        let gone = [
                            crate::blur::replace(&mut page.banner_blur, Arc::new(banner), now),
                            crate::blur::replace(&mut page.blur, Arc::new(wash), now),
                        ];
                        for old in gone.into_iter().flatten() {
                            crate::covers::drop_unshared(old, None, cx);
                        }
                        page.stale_blurs = false;
                    }
                    // A picture the server cannot make (one in a format it
                    // does not read) is no picture: the page takes its
                    // colour from their name, rather than no colour.
                    Err(_) if page.stale_blurs => {
                        page.soft_blurs(banner_radius, wash_radius, None, cx);
                    }
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Moved to a display of another scale: the face read again for it, and
    /// the portrait when the server has it larger. Each shown is kept until
    /// its sharper one comes; the blurs do not depend on the scale. Returns
    /// whether the portrait was asked for.
    fn sharpen(
        &mut self,
        art: Option<Id>,
        scale: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let before = self.pictured_scale;
        self.pictured_scale = scale;
        // A soft face, as with no picture or one that cannot be read, is
        // made again for it; a picture's is read again, and replaces it.
        if self.face.as_ref().is_some_and(theme::soft_holds) {
            self.soft_face(Some(window), cx);
        }
        let Some(art) = art else { return false };
        self.read_face(art, scale, cx);
        let portrait_px = |scale: f32| self.net.bucket((2. * 380. * scale).ceil() as u32);
        let sharper = !self.picture_failed && portrait_px(before) != portrait_px(scale);
        if sharper {
            self.read_portrait(art, scale, cx);
        }
        sharper
    }

    /// The face that stands in for a picture: soft colour from their name.
    /// A read of a face still to come is let go.
    fn soft_face(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.face_read += 1;
        self.stale_face = false;
        let face = theme::soft_cached(&self.name, (28. * self.pictured_scale).ceil() as u32);
        if let Some(old) = self.face.replace(face) {
            crate::covers::drop_unshared(old, window, cx);
        }
    }

    /// The face for the strip, read for the display's `scale`. Where another
    /// picture's face is shown, or none, one that cannot be read gives soft
    /// colour from their name; otherwise what is shown stays.
    fn read_face(&mut self, art: Id, scale: f32, cx: &mut Context<Self>) {
        self.face_read += 1;
        let read = self.face_read;
        let since = self.returns;
        self.in_flight_pictures.face = true;
        #[cfg(test)]
        {
            self.pictures_in_flight += 1;
        }
        self.fetch(
            art,
            (28. * scale).ceil() as u32,
            cx,
            move |page, image, cx| {
                #[cfg(test)]
                {
                    page.pictures_in_flight -= 1;
                }
                let Some(image) = image else {
                    return;
                };
                // Asked for before the last: for a display the window has
                // left since, or another picture.
                if page.face_read != read {
                    return;
                }
                page.in_flight_pictures.face = false;
                page.came_back(Picture::Face, matches!(image, Err(NoArt::Failed)), since);
                let Ok(image) = image else {
                    if page.stale_face {
                        page.soft_face(None, cx);
                    }
                    return;
                };
                page.stale_face = false;
                if let Some(old) = page.face.replace(image) {
                    crate::covers::drop_unshared(old, None, cx);
                }
            },
        );
    }

    /// Their portrait, read for the display's `scale`. Read again sharper for
    /// another display, the one shown stays until one is made from it, and
    /// stays if it cannot be read.
    fn read_portrait(&mut self, art: Id, scale: f32, cx: &mut Context<Self>) {
        self.portrait_read += 1;
        let asked = self.portrait_read;
        let generation = self.fetch_gen;
        let since = self.returns;
        self.in_flight_pictures.portrait = true;
        #[cfg(test)]
        {
            self.pictures_in_flight += 1;
        }
        // Their portrait stands at most 380 points wide at the banner's right
        // end; with none, the cover their picture is stands in for it, so that
        // every artist's circle on the wall has somewhere to fly to. It is
        // kept at its own shape, and cropped to its box as it is made. It is
        // asked for at twice the box's widest, so that a picture up to twice
        // as tall as it is wide still covers the box sharply, as far as the
        // server's largest size allows.
        let http = Arc::clone(&self.net.art);
        let bucket = self.net.bucket((2. * 380. * scale).ceil() as u32);
        let read = self.net.rt().spawn(async move {
            let bytes = http.get(art, bucket).await?;
            crate::covers::decode(move || crate::covers::decode_picture(&bytes))
                .await
                .ok_or(NoArt::Missing)
        });
        self.fetches.push(read.abort_handle());
        cx.spawn(async move |this, cx| {
            let source = read.await.ok();
            this.update(cx, |page, cx| {
                #[cfg(test)]
                {
                    page.pictures_in_flight -= 1;
                }
                let Some(source) = source else {
                    return;
                };
                // Too late: the page left, shows another picture now, or a later
                // read was asked for, for another display.
                if page.fetch_gen != generation
                    || page.pictured_for.map(|(at, _)| at) != Some(Some(art))
                    || page.portrait_read != asked
                {
                    return;
                }
                page.in_flight_pictures.portrait = false;
                let again = page.portrait.is_some();
                match source {
                    Ok(source) => {
                        // The same picture read sharper is made again from
                        // this at the next render; the one shown stays until then.
                        if let Some(made) = page.melted.as_mut().filter(|_| again) {
                            made.0 = (0, 0);
                        }
                        page.portrait = Some(Arc::new(source));
                        page.picture_failed = false;
                        page.came_back(Picture::Portrait, false, since);
                    }
                    // Read sharper for another display: the one shown stays.
                    Err(failed) if again => {
                        page.came_back(Picture::Portrait, failed == NoArt::Failed, since);
                    }
                    Err(failed) => {
                        page.forget_portrait(None, cx);
                        page.picture_failed = true;
                        page.came_back(Picture::Portrait, failed == NoArt::Failed, since);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// With no picture, soft colour from their name: the face, and the
    /// blurs for the banner and the wash.
    /// While the window draws, it is handed `window`, to let go of the face
    /// and the blurs this replaces.
    fn soft_pictures(
        &mut self,
        banner_radius: usize,
        wash_radius: usize,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.soft_blurs(banner_radius, wash_radius, window.as_deref_mut(), cx);
        self.soft_face(window, cx);
    }

    /// Soft colour from their name for the banner's blur and the wash's.
    fn soft_blurs(
        &mut self,
        banner_radius: usize,
        wash_radius: usize,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.stale_blurs = false;
        let soft = theme::soft_pixels(&self.name, crate::blur::SIDE);
        let tone = crate::blur::wash_tone(theme::dark());
        let now = Instant::now();
        let banner = crate::blur::pixels(&soft, banner_radius, crate::blur::BANNER.1);
        let wash = crate::blur::toned(&soft, wash_radius, tone.0, tone.1);
        // The blurs they replace are let go, as the face is.
        let gone = [
            crate::blur::replace(&mut self.banner_blur, Arc::new(banner), now),
            crate::blur::replace(&mut self.blur, Arc::new(wash), now),
        ];
        for old in gone.into_iter().flatten() {
            crate::covers::drop_unshared(old, window.as_deref_mut(), cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (_, viewport) = viewport_of(window);
        let max = (self.height.get() - viewport).max(0.);
        self.elastic
            .scroll(&mut self.top, crate::elastic::Wheel::of(event), max);
        cx.notify();
    }

    /// Whether it is ready to be seen: what it shows read, and its picture there.
    pub(crate) fn ready(&self) -> bool {
        self.page.as_ref().is_some_and(|page| {
            (page.artist.art_id.is_none() || self.blur.is_some())
                && (page.artist.art_id.is_none() || self.melted.is_some() || self.picture_failed)
        }) || self.problem
    }

    /// Whether the artist has a picture to stand at the banner's right end:
    /// their portrait, or the cover that stands in for it.
    pub(crate) fn expects_portrait(&self) -> bool {
        self.page
            .as_ref()
            .is_some_and(|page| page.artist.art_id.is_some())
            && !self.picture_failed
    }

    /// The portrait as the banner shows it, once it is made.
    pub(crate) fn portrait(&self) -> Option<Arc<RenderImage>> {
        self.melted.as_ref().map(|(_, image, _)| Arc::clone(image))
    }

    /// What its colour is borrowed from: the same again changes nothing.
    pub(crate) fn wash_key(&self) -> String {
        match self.page.as_ref().and_then(|page| page.artist.art_id) {
            Some(art) => format!("art:{art}"),
            None => format!("artist:{}", self.artist_id),
        }
    }

    /// Scrolls the page by `dy` at once, as a script's jump does.
    pub(crate) fn jump(&mut self, dy: f32, viewport: f32) {
        let max = (self.height.get() - viewport).max(0.);
        self.top = (self.top + dy).clamp(0., max);
    }

    /// A click on the `n`th of their records, as a script asks.
    pub(crate) fn open_record(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(tile) = self.shelves.iter().flat_map(|(_, records)| records).nth(n) else {
            return;
        };
        let row = Arc::new(TileRow::of_album(tile.clone()));
        let image = tile
            .art_id
            .and_then(|art| self.covers.iter().find(|((id, _), _)| *id == art))
            .map(|(_, (image, _, _))| Arc::clone(image));
        let lift = record::Lift::of(&self.landings, row.id, image);
        cx.emit(ArtistEvent::Open(row, lift));
    }

    /// Up to all the artists, as the banner's way back goes.
    pub(crate) fn up(&mut self, cx: &mut Context<Self>) {
        cx.emit(ArtistEvent::Up(self.artist_id));
    }

    /// Plays every song of theirs, record by record, or shuffled.
    pub(crate) fn play_all(&mut self, shuffle: bool, cx: &mut Context<Self>) {
        let many = Many::Artist {
            id: self.artist_id,
            name: self.name.to_string(),
        };
        let doing = if shuffle { Doing::Shuffle } else { Doing::Play };
        self.player
            .update(cx, |player, cx| player.many(many, doing, cx));
    }

    /// One record of a shelf.
    #[allow(clippy::too_many_arguments)]
    fn card(
        &mut self,
        id: (&'static str, usize),
        tile: &AlbumTile,
        side: f32,
        words: Words,
        appears: bool,
        fetch: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let scale = window.scale_factor();
        let title = tile.title.clone().unwrap_or_else(|| "Untitled".to_owned());
        let credit = tile
            .artist_credit
            .as_ref()
            .map(|credit| credit.text.clone());
        // The line under it: the year it first came out and how many
        // tracks; for a record of others, whose it is.
        let sub = if appears {
            [credit.clone(), tile.year.map(|year| year.to_string())]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
        } else {
            [
                tile.year.map(|year| year.to_string()),
                (tile.track_count > 0).then(|| plural(tile.track_count, "track")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
        };
        let picture = match tile.art_id {
            // The first shelf's covers are read at 220 points, the rest at 140.
            // Cards far outside the viewport keep no image reference so their
            // covers age out and stay inside the page's cover byte budget.
            Some(art) if fetch => {
                let read_at = if side > CARD { 220. } else { 140. };
                let px = (read_at * scale).ceil() as u32;
                self.want(art, px, cx)
            }
            Some(_) => None,
            None => fetch.then(|| {
                theme::soft_cached(
                    &format!("{title}{}", credit.unwrap_or_default()),
                    (side * scale).ceil() as u32,
                )
            }),
        };
        let row = Arc::new(TileRow::of_album(tile.clone()));
        let landings = Rc::clone(&self.landings);
        let lifted = picture.clone();
        let carried = picture.clone();
        let card = record::card(
            id,
            record::Card {
                title: title.into(),
                sub,
                picture,
                side,
                words,
                landing: Some((Rc::clone(&self.landings), row.id)),
                menu: crate::menu::strength(
                    self.menued.as_ref(),
                    &crate::menu::record_key(tile.album),
                ),
            },
            window,
        )
        .on_click(cx.listener(move |_, _, _, cx| {
            let lift = record::Lift::of(&landings, row.id, lifted.clone());
            cx.emit(ArtistEvent::Open(Arc::clone(&row), lift));
        }));
        crate::menu::on_record(crate::carry::record(card, tile, carried), tile)
    }

    /// Their records, each kind taking only the room it needs, as many
    /// side by side as fit across `area` points.
    fn records(
        &mut self,
        area: f32,
        viewport: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let shelves = self.shelves.clone();
        let mut lines: Vec<Vec<(usize, f32)>> = Vec::new();
        let mut used = 0f32;
        for (at, (_, records)) in shelves.iter().enumerate() {
            let step = if at == 0 {
                FIRST_CARD + 20.
            } else {
                CARD + 20.
            };
            let basis = records.len() as f32 * step - 20.;
            let wide = basis.min(area);
            match lines.last_mut() {
                Some(line) if used + SHELF_GAP + wide <= area => {
                    line.push((at, wide));
                    used += SHELF_GAP + wide;
                }
                _ => {
                    lines.push(vec![(at, wide)]);
                    used = wide;
                }
            }
        }
        let view_min = self.top - viewport;
        let view_max = self.top + 2. * viewport;
        let head = self.head.get();
        let use_measured = head.is_finite() && (self.measured_area - area).abs() <= 1.;
        let mut y_min = banner::HEIGHT + banner::BELOW;
        let mut y_max = banner::HEIGHT + banner::BELOW;
        let mut out = div().flex().flex_col().gap(px(PART_GAP));
        for line in lines {
            let mut row = div().flex().items_start().gap(px(SHELF_GAP));
            let mut line_min = 0f32;
            let mut line_max = 0f32;
            for (at, wide) in line {
                let Some((shelf, records)) = shelves.get(at) else {
                    continue;
                };
                let first = at == 0;
                let track = if first { FIRST_CARD } else { CARD };
                // The room left over goes between the records.
                let columns = (((wide + 20.) / (track + 20.)).floor() as usize).max(1);
                let gap = if columns > 1 {
                    (wide - columns as f32 * track) / (columns - 1) as f32
                } else {
                    20.
                };
                let words = if first {
                    Words {
                        title: 14.5,
                        sub: 13.,
                        weight: 600.,
                    }
                } else {
                    Words {
                        title: 13.5,
                        sub: 12.5,
                        weight: 600.,
                    }
                };
                let row_gap = if first { 26. } else { 22. };
                let min_step = track + 26. + row_gap;
                let max_step = track + 76. + row_gap;
                let heading_h = 40.;
                let shelf_rows = records.len().div_ceil(columns);
                line_min = line_min.max(heading_h + shelf_rows as f32 * min_step);
                line_max = line_max.max(heading_h + shelf_rows as f32 * max_step);
                // The rows are made here, `columns` to a row, rather than
                // left to wrapping: the gap is rounded to the screen's
                // pixels, and a row rounded a hair wider than `wide` would
                // wrap its last record and leave a hole where it stood.
                let mut cards = div().w(px(wide)).flex().flex_col().gap(px(row_gap));
                let mut cards_row = div().flex().gap(px(gap));
                let appears = *shelf == Shelf::AppearsOn;
                for (index, tile) in records.iter().enumerate() {
                    if index > 0 && index % columns == 0 {
                        cards = cards.child(cards_row);
                        cards_row = div().flex().gap(px(gap));
                    }
                    let near = if use_measured
                        && let Some(bounds) = self.landings.bounds(tile.album.id())
                    {
                        let card_top = f32::from(bounds.origin.y) - head;
                        let card_bottom = card_top + f32::from(bounds.size.height);
                        card_bottom >= view_min && card_top <= view_max
                    } else {
                        let row_index = (index / columns) as f32;
                        let row_top_min = y_min + heading_h + row_index * min_step;
                        let row_bottom_max = y_max + heading_h + (row_index + 1.) * max_step;
                        row_bottom_max >= view_min && row_top_min <= view_max
                    };
                    cards_row = cards_row.child(self.card(
                        (shelf_id(*shelf), index),
                        tile,
                        track,
                        words,
                        appears,
                        near,
                        window,
                        cx,
                    ));
                }
                cards = cards.child(cards_row);
                row = row.child(
                    div()
                        .w(px(wide))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .mb(px(14.))
                                .child(record::heading(shelf_heading(*shelf), window)),
                        )
                        .child(cards),
                );
            }
            y_min += line_min + PART_GAP;
            y_max += line_max + PART_GAP;
            out = out.child(row);
        }
        self.measured_area = area;
        out
    }

    /// Your history with them: the picture of your years, and the records
    /// you played most, under it or, `across`, beside it. With no dated
    /// play there is no picture, but the records still rank, undated plays
    /// and all. `None` when there is neither.
    fn history(
        &mut self,
        history: &HistoryOf,
        across: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let title = format!("You and {}", self.name);
        let color = self.page.as_ref().and_then(|page| page.art_color);
        let picture = pictured(Some(history)).map(|(history, span)| {
            record::years(title.clone(), &history.years, span, color, window)
        });
        let records =
            (!history.records.is_empty()).then(|| self.played(&history.records, window, cx));
        Some(match (picture, records) {
            (None, None) => return None,
            (Some(picture), None) => picture,
            (None, Some(records)) => div()
                .flex()
                .flex_col()
                .child(div().mb(px(14.)).child(record::heading(title, window)))
                .child(records),
            (Some(picture), Some(records)) if across => div()
                .flex()
                .items_start()
                .gap(px(48.))
                .child(div().flex_1().max_w(px(620.)).child(picture))
                .child(
                    div()
                        .min_w(px(260.))
                        .max_w(px(420.))
                        .flex_1()
                        .mt(px(39.))
                        .child(records),
                ),
            (Some(picture), Some(records)) => picture.child(div().mt(px(14.)).child(records)),
        })
    }

    /// The records played most, each with the year it was most yours.
    fn played(
        &mut self,
        records: &[RecordPlayed],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let scale = window.scale_factor();
        let mut list = div()
            .max_w(px(620.))
            .mx(px(-8.))
            .flex()
            .flex_col()
            .gap(px(1.));
        for (at, played) in records.iter().enumerate() {
            let tile = &played.tile;
            let title = tile.title.clone().unwrap_or_else(|| "Untitled".to_owned());
            let picture = match tile.art_id {
                Some(art) => self.want(art, (30. * scale).ceil() as u32, cx),
                None => Some(theme::soft_cached(
                    &format!(
                        "{title}{}",
                        tile.artist_credit
                            .as_ref()
                            .map_or("", |credit| credit.text.as_str())
                    ),
                    (30. * scale).ceil() as u32,
                )),
            };
            let row = Arc::new(TileRow::of_album(tile.clone()));
            let row_id = row.id;
            let landings = Rc::clone(&self.landings);
            let lifted = picture.clone();
            // A record played most, as a record has its menu; while it is
            // open the row wears the menu's fill.
            let shown =
                crate::menu::strength(self.menued.as_ref(), &format!("played:{}", tile.album.id()));
            let (album, named, credit) = (tile.album, title.clone(), tile.artist_credit.clone());
            list = list.child(
                div()
                    .id(("played", at))
                    .h(px(42.))
                    .px(px(8.))
                    .rounded(px(9.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(13.5))
                    .hover(theme::hovered)
                    .active(theme::pressed)
                    .when(shown > 0., |row| row.bg(crate::menu::fill(shown)))
                    .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                        cx.stop_propagation();
                        let entries =
                            crate::menu::record_entries(album, &named, credit.as_ref(), cx);
                        let key = format!("played:{}", album.id());
                        crate::menu::popup(key, entries, event.position, window, cx);
                    })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let lift = record::Lift::of_elsewhere(&landings, row.id, lifted.clone());
                        cx.emit(ArtistEvent::Open(Arc::clone(&row), lift));
                    }))
                    .child(
                        div()
                            .relative()
                            .size(px(30.))
                            .flex_none()
                            .rounded(px(5.))
                            .bg(theme::fill())
                            .children(picture.map(|picture| {
                                img(picture)
                                    .size(px(30.))
                                    .rounded(px(5.))
                                    .object_fit(ObjectFit::Cover)
                            }))
                            .child(self.landings.mark_elsewhere(row_id)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight(550.))
                            .child(title),
                    )
                    .children(played.peak_year.map(|year| {
                        div()
                            .flex_none()
                            .text_size(px(12.5))
                            .text_color(theme::faint())
                            .font_features(crate::bar::tabular())
                            .child(format!("most in {year}"))
                    })),
            );
        }
        list
    }
}

fn shelf_id(shelf: Shelf) -> &'static str {
    match shelf {
        Shelf::Albums => "albums",
        Shelf::Singles => "singles",
        Shelf::Live => "live",
        Shelf::Collections => "collections",
        Shelf::AppearsOn => "appears-on",
    }
}

fn gutter(width: f32) -> f32 {
    ((width - MEASURE) / 2.).round().max(40.)
}

impl Render for Artist {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.frame += 1;
        self.covers_scale = window.scale_factor();
        crate::waits::frame("artist");
        self.pictures(window, cx);
        self.menued = crate::menu::marked(window, cx);
        let (width, viewport) = viewport_of(window);
        // The portrait for its box in the banner, made again when that changes.
        let pixels = window.scale_factor();
        self.melt(
            (
                (banner::portrait_wide(width) * pixels).round() as u32,
                (banner::HEIGHT * pixels).round() as u32,
            ),
            cx,
        );
        // With Up next moving the page over, the page keeps to the room left
        // of the card; the banner still runs to the window's edge.
        let rail = crate::upnext::rail(cx);
        let gutter = gutter(width - rail);
        let content = width - rail - 2. * gutter;
        let max = (self.height.get() - viewport).max(0.);
        self.top = self.top.clamp(0., max);
        let mut animating = self.elastic.step(Instant::now());
        let top = self.top + self.elastic.shown();

        let mut body = div()
            .pl(px(gutter))
            .pr(px(gutter + rail))
            .w(px(width))
            .flex()
            .flex_col();
        let history = self.history.clone();
        if self.problem {
            body = body.child(
                div()
                    .my(px(8.))
                    .text_size(px(13.5))
                    .text_color(theme::faint())
                    .child("This artist could not be read."),
            );
        } else if self.page.is_some() {
            let counts: Vec<usize> = self
                .shelves
                .iter()
                .map(|(_, records)| records.len())
                .collect();
            let beside = history_beside(content, &counts, has_history(history.as_ref()));
            let across = !beside && content >= HISTORY_ACROSS;
            let column = history_column(content);
            let area = if beside {
                content - HISTORY_GAP - column
            } else {
                content
            };
            let records = self.records(area, viewport, window, cx);
            let songs = if self.tracks.read(cx).is_empty() {
                None
            } else {
                // As many columns as fit at 480 points or more, running down
                // each in turn; in one, no wider than 920.
                let columns = song_columns(area);
                self.tracks.update(cx, |tracks, _| {
                    tracks.columns = columns;
                    tracks.width = if columns > 1 { area } else { area.min(920.) };
                });
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .mb(px(14.))
                                .child(record::heading("Other songs", window)),
                        )
                        .child(self.tracks.clone()),
                )
            };
            let history_part = history
                .as_ref()
                .and_then(|history| self.history(history, across, window, cx));
            if beside {
                // The column stays under the banner's strip as the records
                // scroll, until what it stands beside has gone by.
                let natural = banner::HEIGHT + banner::BELOW;
                let room = (self.beside_height.get() - self.column.get()).max(0.);
                let stays = (self.top + STAYS_AT - natural).clamp(0., room);
                let column_height = Rc::clone(&self.column);
                let left_height = Rc::clone(&self.beside_height);
                body = body.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(HISTORY_GAP))
                        .child(
                            div()
                                .relative()
                                .w(px(area))
                                .flex()
                                .flex_col()
                                .gap(px(PART_GAP))
                                .child(records)
                                .children(songs)
                                .child(
                                    canvas(
                                        move |bounds, _, _| {
                                            left_height.set(f32::from(bounds.size.height))
                                        },
                                        |_, (), _, _| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                ),
                        )
                        .children(history_part.map(|part| {
                            div()
                                .relative()
                                .top(px(stays))
                                .w(px(column))
                                .child(part)
                                .child(
                                    canvas(
                                        move |bounds, _, _| {
                                            column_height.set(f32::from(bounds.size.height))
                                        },
                                        |_, (), _, _| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                )
                        })),
                );
            } else {
                body = body
                    .child(records)
                    .children(history_part.map(|part| div().mt(px(PART_GAP)).child(part)))
                    .children(songs.map(|songs| songs.mt(px(PART_GAP))));
            }
        }

        // The page measures itself: from a mark at its head to one at its foot.
        let height = Rc::clone(&self.height);
        let head = Rc::clone(&self.head);
        let start = Rc::clone(&self.head);
        let page_area = div()
            .absolute()
            .left_0()
            .top(px(-top + self.look.hop))
            .opacity(self.look.body)
            .w(px(width))
            .flex()
            .flex_col()
            .child(
                canvas(
                    move |bounds, _, _| start.set(f32::from(bounds.origin.y)),
                    |_, (), _, _| {},
                )
                .h(px(0.))
                .w_full(),
            )
            .child(div().h(px(banner::HEIGHT + banner::BELOW)).flex_none())
            .child(body)
            .child(div().h(px(48.)).flex_none())
            .child(
                canvas(
                    move |bounds, _, _| height.set(f32::from(bounds.origin.y) - head.get()),
                    |_, (), _, _| {},
                )
                .h(px(0.))
                .w_full(),
            );

        let artist = self.page.as_ref();
        let facts = artist.map_or_else(String::new, |artist| {
            let counts: Vec<(Shelf, u64)> = self
                .shelves
                .iter()
                .map(|(shelf, records)| (*shelf, records.len() as u64))
                .collect();
            artist_facts(&counts, artist.artist.track_count)
        });
        let (sentence, faint) = said(history.as_ref());
        let banner = Banner {
            title: self.name.clone(),
            facts,
            up_label: "Artists".into(),
            sentence,
            faint,
            playable: artist.is_some_and(|artist| artist.artist.track_count > 0),
            round: true,
            backdrop: self.banner_blur.clone(),
            color: theme::deep(artist.and_then(|artist| artist.art_color)),
            face: self.face.clone(),
            hung: None,
            portrait: self
                .melted
                .as_ref()
                .filter(|_| !self.portrait_away)
                .map(|(_, image, since)| (Arc::clone(image), *since)),
            picture: (self.look.picture, self.look.scale),
        };
        let actions = Actions {
            up: Rc::new(|page: &mut Self, cx: &mut Context<Self>| page.up(cx)),
            play: Rc::new(|page: &mut Self, shuffle, cx: &mut Context<Self>| {
                page.play_all(shuffle, cx);
            }),
        };
        let (banner, coming) =
            banner.render(top, width, gutter, gutter + rail, &actions, window, cx);
        animating |= coming;
        if animating {
            window.request_animation_frame();
        }
        div()
            .id("artist")
            .relative()
            .size_full()
            .overflow_hidden()
            .on_scroll(cx.listener(Self::on_scroll))
            .child(crate::frost::faded(self.look.body, page_area))
            .child(
                div()
                    .absolute()
                    .size_full()
                    .opacity(self.look.banner)
                    .child(crate::frost::faded(self.look.banner, banner)),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;
    use crate::link::{Link, LinkEvent};

    const ARTIST: Id = Id::from_bytes([1; 16]);
    /// Their picture, which the pictures' server has.
    const PICTURE: Id = Id::from_bytes([2; 16]);
    /// A picture the pictures' server has none of.
    const NONE: Id = Id::from_bytes([3; 16]);
    /// The sizes the pictures' server makes: the face is read at the
    /// smallest, the portrait at the largest, and the blurs at theirs.
    const SIZES: [u32; 3] = [64, crate::blur::SIDE, 1024];
    const FACE: u32 = 64;
    const PORTRAIT: u32 = 1024;
    const BLURS: u32 = crate::blur::SIDE;

    /// A server of pictures on loopback, over HTTP as the app reads them.
    /// It has a JPEG of every picture but `NONE`, and fails a request for
    /// a size in `failing` when it comes, as a satellite does whose hub has
    /// gone. It holds its answer to a size in `held` until that size is
    /// let go. It counts the requests for each picture at each size.
    #[derive(Clone)]
    struct Pictures {
        asked: Arc<Mutex<HashMap<(String, u32), usize>>>,
        unanswered: Arc<AtomicUsize>,
        failing: Arc<Mutex<HashSet<u32>>>,
        held: Arc<tokio::sync::watch::Sender<HashSet<u32>>>,
    }

    impl Pictures {
        /// Starts it, and says where it listens.
        fn start() -> (Self, std::net::SocketAddr) {
            let pictures = Self {
                asked: Arc::default(),
                unanswered: Arc::default(),
                failing: Arc::default(),
                held: Arc::new(tokio::sync::watch::Sender::new(HashSet::new())),
            };
            let rt = crate::net::rt();
            let bound = rt.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                Ok::<_, std::io::Error>((listener.local_addr()?, listener))
            });
            assert!(bound.is_ok(), "no port for the pictures");
            let Ok((address, listener)) = bound else {
                return (pictures, std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
            };
            let serving = pictures.clone();
            rt.spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serving.clone().serve(stream));
                }
            });
            (pictures, address)
        }

        /// Answers each request on one connection, kept alive.
        async fn serve(self, stream: tokio::net::TcpStream) {
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            loop {
                let mut first = String::new();
                if read.read_line(&mut first).await.unwrap_or(0) == 0 {
                    return;
                }
                loop {
                    let mut header = String::new();
                    if read.read_line(&mut header).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if header == "\r\n" {
                        break;
                    }
                }
                // `GET /v1/art/{art}/{size}?token= HTTP/1.1`
                let path = first.split(' ').nth(1).unwrap_or_default();
                let path = path.split('?').next().unwrap_or_default();
                let mut parts = path.rsplit('/');
                let size: u32 = parts.next().and_then(|size| size.parse().ok()).unwrap_or(0);
                let art = parts.next().unwrap_or_default().to_owned();
                // What it answers is settled as the request comes, before
                // it is counted: a test that sees it counted can change
                // what the next is answered.
                let (status, body) = if art == NONE.to_string() {
                    ("404 Not Found", Vec::new())
                } else if lock(&self.failing).contains(&size) {
                    ("503 Service Unavailable", Vec::new())
                } else {
                    ("200 OK", jpeg())
                };
                let mut held = self.held.subscribe();
                self.unanswered.fetch_add(1, Ordering::SeqCst);
                *lock(&self.asked).entry((art.clone(), size)).or_default() += 1;
                let _ = held.wait_for(|held| !held.contains(&size)).await;
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let sent = write.write_all(head.as_bytes()).await.is_ok()
                    && write.write_all(&body).await.is_ok();
                self.unanswered.fetch_sub(1, Ordering::SeqCst);
                if !sent {
                    return;
                }
            }
        }

        fn fail(&self, sizes: &[u32]) {
            *lock(&self.failing) = sizes.iter().copied().collect();
        }

        fn hold(&self, sizes: &[u32]) {
            self.held.send_replace(sizes.iter().copied().collect());
        }

        /// How often `art` was asked for at `size`.
        fn asked(&self, art: Id, size: u32) -> usize {
            lock(&self.asked)
                .get(&(art.to_string(), size))
                .copied()
                .unwrap_or(0)
        }

        fn all(&self) -> usize {
            lock(&self.asked).values().sum()
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A small square picture, as the server makes them.
    fn jpeg() -> Vec<u8> {
        let picture = image::RgbImage::from_pixel(64, 64, image::Rgb([180, 110, 70]));
        let mut bytes = Vec::new();
        let encoded = image::codecs::jpeg::JpegEncoder::new(&mut bytes).encode_image(&picture);
        assert!(encoded.is_ok(), "the picture could not be made");
        bytes
    }

    /// A session with the artist's page to read, whose picture is `art`,
    /// on a satellite whose hub answers or not, reading pictures from
    /// `pictures`.
    fn session(art: Id, answers: bool, pictures: std::net::SocketAddr) -> Net {
        let (send, mut calls) = tokio::sync::mpsc::channel::<String>(64);
        let (answer, receive) = tokio::sync::mpsc::channel(64);
        let _runtime = crate::net::rt().enter();
        let client = Arc::new(ritornel_client::Client::over_memory(send, receive));
        crate::net::rt().spawn(async move {
            while let Some(text) = calls.recv().await {
                let Ok(ritornel_contract::Frame::Request(request)) =
                    ritornel_contract::Frame::decode(&text)
                else {
                    continue;
                };
                let outcome = match request.method.as_str() {
                    "artist.get" => Ok(json!({
                        "artistId": ARTIST.to_string(),
                        "name": "Invented Artist",
                        "sortName": null,
                        "albumCount": 0,
                        "trackCount": 0,
                        "artId": art.to_string(),
                        "portrait": true,
                        "artColor": null,
                        "libraryRevision": 1,
                    })),
                    "list.open" => Ok(json!({
                        "view": "view", "total": 0, "libraryRevision": 1, "offset": 0, "rows": [],
                    })),
                    "list.sections" => Ok(json!({ "sections": [] })),
                    "list.close" => Ok(json!({ "closed": true })),
                    _ => Err(ritornel_contract::RpcError::new(
                        ritornel_contract::ErrorCode::Unsupported,
                        "not in this test",
                    )),
                };
                let frame = ritornel_contract::Frame::Response(ritornel_contract::Response {
                    id: Some(request.id),
                    outcome,
                });
                if answer.send(frame.encode()).await.is_err() {
                    return;
                }
            }
        });
        Net::satellite(client, answers).with_art(pictures, SIZES.to_vec())
    }

    /// The artist's page in a window, and the link it follows.
    fn open(
        net: Net,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<Artist>, Entity<Link>, &mut gpui::VisualTestContext) {
        // The pictures come from the app's own network runtime, as they do
        // when it runs: from another thread. The clock stays the test's.
        cx.executor().allow_parking();
        let link = cx.update(crate::link::unlinked);
        let followed = link.clone();
        let (page, cx) = cx.add_window_view(move |_, cx| {
            let player = cx.new(|cx| Player::new(&followed, cx));
            Artist::new(
                Arc::new(net),
                &followed,
                player,
                ARTIST,
                "Invented Artist".into(),
                cx,
            )
        });
        (page, link, cx)
    }

    /// Runs the window until `done` holds of the page.
    fn until(
        page: &Entity<Artist>,
        cx: &mut gpui::VisualTestContext,
        what: &str,
        done: impl Fn(&Artist) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            assert!(Instant::now() < deadline, "the page never {what}");
            cx.run_until_parked();
            if page.read_with(cx, |page, _| done(page)) {
                return;
            }
            std::thread::yield_now();
        }
    }

    /// Runs the window until every picture asked for has come back to the
    /// page and been taken in.
    fn settle(page: &Entity<Artist>, pictures: &Pictures, cx: &mut gpui::VisualTestContext) {
        until(page, cx, "took in its pictures", |page| {
            page.pictures_in_flight == 0 && pictures.unanswered.load(Ordering::SeqCst) == 0
        });
    }

    /// A minute of the test clock, a frame drawn each second, and whatever
    /// the page asked meanwhile taken in.
    fn wait_a_minute(page: &Entity<Artist>, pictures: &Pictures, cx: &mut gpui::VisualTestContext) {
        for _ in 0..60 {
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.update(|window, _| window.refresh());
            settle(page, pictures, cx);
        }
    }

    fn say(link: &Entity<Link>, net: Net, cx: &mut gpui::VisualTestContext) {
        link.update(cx, |_, cx| cx.emit(LinkEvent::Restated(Arc::new(net))));
        cx.update(|window, _| window.refresh());
    }

    /// Whether the page shows their face as read, not soft colour.
    fn face_read(page: &Artist) -> bool {
        page.face
            .as_ref()
            .is_some_and(|face| !theme::soft_holds(face))
    }

    fn unread(face: bool, portrait: bool, blurs: bool) -> Unread {
        Unread {
            face,
            portrait,
            blurs,
        }
    }

    #[gpui::test]
    fn pictures_that_failed_while_the_hub_was_away_are_read_again_once(
        cx: &mut gpui::TestAppContext,
    ) {
        let (pictures, address) = Pictures::start();
        // The hub is away: the portrait and the blurs fail, the face is read.
        pictures.fail(&[PORTRAIT, BLURS]);
        let (page, link, cx) = open(session(PICTURE, false, address), cx);
        until(&page, cx, "asked for its pictures", |_| pictures.all() == 3);
        settle(&page, &pictures, cx);
        assert_eq!(
            page.read_with(cx, |page, _| page.unread),
            unread(false, true, true)
        );
        wait_a_minute(&page, &pictures, cx);
        assert_eq!(pictures.all(), 3);

        // Back: the two that failed are asked for again, once; the face,
        // which was read, is not, and stays.
        pictures.fail(&[]);
        say(&link, session(PICTURE, true, address), cx);
        wait_a_minute(&page, &pictures, cx);
        assert_eq!(pictures.asked(PICTURE, FACE), 1);
        assert_eq!(pictures.asked(PICTURE, PORTRAIT), 2);
        assert_eq!(pictures.asked(PICTURE, BLURS), 2);
        let (shown, failed, left) = page.read_with(cx, |page, _| {
            (
                face_read(page) && page.portrait.is_some(),
                page.picture_failed,
                page.unread,
            )
        });
        assert!(shown && !failed);
        assert_eq!(left, Unread::default());

        // Away and back again with every picture read: nothing is asked.
        say(&link, session(PICTURE, false, address), cx);
        say(&link, session(PICTURE, true, address), cx);
        wait_a_minute(&page, &pictures, cx);
        assert_eq!(pictures.all(), 5);
    }

    #[gpui::test]
    fn a_picture_that_fails_again_is_asked_for_once_each_return(cx: &mut gpui::TestAppContext) {
        let (pictures, address) = Pictures::start();
        pictures.fail(&[FACE, PORTRAIT, BLURS]);
        let (page, link, cx) = open(session(PICTURE, false, address), cx);
        until(&page, cx, "asked for its pictures", |_| pictures.all() == 3);
        settle(&page, &pictures, cx);
        assert_eq!(
            page.read_with(cx, |page, _| page.unread),
            unread(true, true, true)
        );

        // Back, but the pictures still fail: each is asked for once, and
        // not again however long the page waits.
        say(&link, session(PICTURE, true, address), cx);
        wait_a_minute(&page, &pictures, cx);
        assert_eq!(pictures.all(), 6);
        assert_eq!(
            page.read_with(cx, |page, _| page.unread),
            unread(true, true, true)
        );

        // Away and back once more, and they are read.
        pictures.fail(&[]);
        say(&link, session(PICTURE, false, address), cx);
        say(&link, session(PICTURE, true, address), cx);
        wait_a_minute(&page, &pictures, cx);
        for size in [FACE, PORTRAIT, BLURS] {
            assert_eq!(pictures.asked(PICTURE, size), 3);
        }
        let shown = page.read_with(cx, |page, _| face_read(page) && page.portrait.is_some());
        assert!(shown);
    }

    #[gpui::test]
    fn a_read_that_fails_after_the_hub_is_back_is_asked_for_again(cx: &mut gpui::TestAppContext) {
        let (pictures, address) = Pictures::start();
        // The portrait's read is under way as the hub comes back, and fails
        // after: it was asked of the hub that went.
        pictures.fail(&[PORTRAIT]);
        pictures.hold(&[PORTRAIT]);
        let (page, link, cx) = open(session(PICTURE, false, address), cx);
        until(&page, cx, "asked for its pictures", |page| {
            pictures.all() == 3 && page.pictures_in_flight == 1
        });
        say(&link, session(PICTURE, true, address), cx);
        cx.run_until_parked();
        pictures.fail(&[]);
        pictures.hold(&[]);
        until(&page, cx, "read its portrait", |page| {
            page.portrait.is_some()
        });
        settle(&page, &pictures, cx);
        assert_eq!(pictures.asked(PICTURE, PORTRAIT), 2);
        assert_eq!(pictures.all(), 4);
    }

    #[gpui::test]
    fn a_picture_the_server_has_none_of_is_not_asked_for_again(cx: &mut gpui::TestAppContext) {
        let (pictures, address) = Pictures::start();
        let (page, link, cx) = open(session(NONE, true, address), cx);
        until(&page, cx, "asked for its pictures", |_| pictures.all() == 3);
        settle(&page, &pictures, cx);
        assert_eq!(page.read_with(cx, |page, _| page.unread), Unread::default());
        for _ in 0..3 {
            say(&link, session(NONE, false, address), cx);
            say(&link, session(NONE, true, address), cx);
            wait_a_minute(&page, &pictures, cx);
        }
        assert_eq!(pictures.all(), 3);
    }

    #[gpui::test]
    fn leaving_during_initial_read_resumes_pictures_on_return(cx: &mut gpui::TestAppContext) {
        let (pictures, address) = Pictures::start();
        pictures.hold(&[FACE, PORTRAIT, BLURS]);
        let (page, _link, cx) = open(session(PICTURE, true, address), cx);
        until(&page, cx, "asked for its pictures", |page| {
            pictures.all() == 3 && page.pictures_in_flight == 3
        });

        // Navigate away while all three header picture reads are still in flight.
        page.update(cx, |page, cx| page.seen(false, cx));
        until(&page, cx, "cancelled reads settled", |page| {
            page.pictures_in_flight == 0
        });
        assert_eq!(page.read_with(cx, |page, _| page.unread), Unread::default());
        assert!(!page.read_with(cx, |page, _| page.picture_failed));

        // Return to the same artist at the same scale: all three reads resume.
        pictures.hold(&[]);
        page.update(cx, |page, cx| {
            page.seen(true, cx);
            cx.notify();
        });
        until(&page, cx, "pictures loaded after returning", |page| {
            face_read(page) && page.portrait.is_some() && !page.stale_blurs
        });
        settle(&page, &pictures, cx);
        assert_eq!(pictures.asked(PICTURE, FACE), 2);
        assert_eq!(pictures.asked(PICTURE, PORTRAIT), 2);
        assert_eq!(pictures.asked(PICTURE, BLURS), 2);
    }

    #[gpui::test]
    fn shelf_cover_cache_evicts_scrolled_past_covers_to_stay_within_budget(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_pictures, address) = Pictures::start();
        let (page, _link, cx) = open(session(NONE, true, address), cx);
        let chunk = MAX_COVER_BYTES / 3;
        let make = || {
            Arc::new(RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::new(1, 1),
            )]))
        };
        page.update(cx, |page, cx| {
            page.covers_scale = 2.;
            for i in 1u8..=6 {
                page.frame += 2;
                page.store_cover(Id::from_bytes([i; 16]), 280, make(), chunk, cx);
            }
            assert!(
                page.cover_bytes <= MAX_COVER_BYTES,
                "expected cover_bytes ({}) <= MAX_COVER_BYTES ({})",
                page.cover_bytes,
                MAX_COVER_BYTES
            );
            assert!(page.covers.len() <= 3);
        });
    }

    /// An invented album of theirs, the `n`th, with no cover.
    fn album(n: usize) -> AlbumTile {
        let mut id = [0xA0; 16];
        id[..8].copy_from_slice(&(n as u64).to_be_bytes());
        AlbumTile {
            album: ritornel_contract::AlbumRef::ReleaseGroup {
                id: Id::from_bytes(id),
            },
            title: Some(format!("Invented Album {n}")),
            artist_credit: None,
            year: Some(2000),
            track_count: 10,
            duration_ms: 0,
            version_count: 1,
            art_id: None,
            quality: None,
        }
    }

    /// Where each of `albums` has its cover with the page `width` points wide.
    fn covers_at(
        page: &Entity<Artist>,
        albums: &[AlbumTile],
        width: f32,
        cx: &mut gpui::VisualTestContext,
    ) -> Vec<gpui::Bounds<gpui::Pixels>> {
        cx.simulate_resize(gpui::size(px(width), px(900.)));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        page.read_with(cx, |page, _| {
            albums
                .iter()
                .map(|tile| {
                    let bounds = page.landings.bounds(tile.album.id());
                    assert!(bounds.is_some(), "no cover drawn at {width}");
                    bounds.unwrap_or_default()
                })
                .collect()
        })
    }

    #[gpui::test]
    fn a_shelf_fills_each_row_at_every_width(cx: &mut gpui::TestAppContext) {
        let (_pictures, address) = Pictures::start();
        let (page, _link, cx) = open(session(NONE, true, address), cx);
        until(&page, cx, "was read", |page| page.page.is_some());
        let step = FIRST_CARD + 20.;
        // Every width from 800 to 1,300 points, where the leftover room
        // between four to six albums seldom comes to whole pixels, and
        // the widest windows; one or three albums never fill a row.
        let every: Vec<f32> = (800..=1300)
            .chain([2000, 2600, 3400])
            .map(|width| width as f32)
            .collect();
        let few = vec![700., 1100., 2000., 3400.];
        for (count, widths) in [(1, &few), (3, &few), (9, &every), (20, &every)] {
            let albums: Vec<AlbumTile> = (0..count).map(album).collect();
            page.update(cx, |page, cx| {
                page.shelves = vec![(Shelf::Albums, albums.clone())];
                page.measured_area = 0.;
                cx.notify();
            });
            for &width in widths {
                let covers = covers_at(&page, &albums, width, cx);
                // With no history beside them, the albums have the page's
                // whole measure.
                let wide = width - 2. * gutter(width);
                let top = covers.first().map(|at| at.origin.y);
                let columns = covers
                    .iter()
                    .take_while(|at| Some(at.origin.y) == top)
                    .count();
                // A row holds every album that fits.
                assert!(
                    columns == count || (columns + 1) as f32 * step - 20. > wide,
                    "{count} albums at {width}: {columns} in the first row, room for more",
                );
                // Each row lies level, its albums under the first row's.
                let first: Vec<_> = covers.iter().take(columns).map(|at| at.origin.x).collect();
                for (at, row) in covers.chunks(columns).enumerate() {
                    let xs: Vec<_> = row.iter().map(|cover| cover.origin.x).collect();
                    assert_eq!(
                        xs,
                        first.get(..row.len()).unwrap_or_default(),
                        "{count} albums at {width}: row {at} out of the columns",
                    );
                    let level = row.first().map(|cover| cover.origin.y);
                    assert!(
                        row.iter().all(|cover| Some(cover.origin.y) == level),
                        "{count} albums at {width}: row {at} not level",
                    );
                }
                // Albums that leave room over sit at their natural spacing.
                if let [one, two, ..] = covers.as_slice()
                    && count as f32 * step - 20. <= wide
                {
                    let spacing = f32::from(two.origin.x - one.origin.x);
                    assert_eq!(spacing, step, "{count} albums at {width}");
                }
            }
        }
    }
}
