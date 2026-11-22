//! An artist's page (`pages/Artist.svelte`): the banner from edge to edge
//! made of their picture (`banner.rs`), with their portrait standing clear
//! at the right, or with none the cover their picture is, and under it
//! everything of theirs,
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
//! What it reads (`pages/read.ts`): `artist.get`, their records and their
//! songs on no record as whole lists read through views closed at once,
//! and `history.artist`; again when the library changes, and after a change
//! of plays alone only their songs and your history with them.

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
    AlbumTile, ArtistGetParams, ArtistPage, ContextKind, Event, FILTER_VERSION, Filter, HistoryOf,
    Id, LibraryChange, ListKind, ListOpenParams, PlayContext, Predicate, Query, RecordPlayed, Row,
    Section, Shelf, SongRow, Sort, SortKey,
};

use crate::banner::{self, Actions, Banner};
use crate::link::{Link, LinkEvent};
use crate::net::Net;
use crate::player::{Doing, Many, Player};
use crate::record::{self, Words};
use crate::theme;
use crate::tracks::Tracks;
use crate::wall::{TileRow, viewport_of};
use crate::words::artist::{
    CARD, FIRST_CARD, HISTORY_ACROSS, HISTORY_GAP, SHELF_GAP, artist_facts, history_beside,
    history_column, shelf_heading, song_columns,
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
    history: Option<HistoryOf>,
    revision: u64,
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
    covers: HashMap<(Id, u32), Arc<RenderImage>>,
    asked: HashSet<(Id, u32)>,
    fetches: crate::covers::FetchBag,
    revision: u64,
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
    pub blur: Option<(Arc<RenderImage>, Instant)>,
    banner_blur: Option<(Arc<RenderImage>, Instant)>,
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
    pub top: f32,
    elastic: crate::elastic::Elastic,
    /// How tall the page was when last drawn, and the column of history
    /// and what it stands beside.
    height: Rc<Cell<f32>>,
    column: Rc<Cell<f32>>,
    beside_height: Rc<Cell<f32>>,
    /// How it shows while it arrives or leaves, as the stage moves it.
    pub look: crate::moves::Look,
    _link: Subscription,
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
        // When the library changes, what the page shows is read again.
        let followed = cx.subscribe(link, |page: &mut Self, _, event: &LinkEvent, cx| {
            if let LinkEvent::Server(Event::Library(event)) = event
                && event.library_revision > page.revision
            {
                let plays = !event.changed.is_empty()
                    && event
                        .changed
                        .iter()
                        .all(|change| *change == LibraryChange::Stats);
                page.read(plays, cx);
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
            asked: HashSet::new(),
            fetches: crate::covers::FetchBag::default(),
            revision: 0,
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
            top: 0.,
            elastic: crate::elastic::Elastic::default(),
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
        for (_, image) in self.covers.drain() {
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

    /// Reads the page, or after a change of plays alone only their songs
    /// and your history with them.
    fn read(&mut self, plays_only: bool, cx: &mut Context<Self>) {
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
            let history = client.call::<HistoryArtist>(&params);
            if plays_only {
                let (loose, history) = tokio::join!(loose, history);
                let loose = loose?;
                return Ok::<_, ritornel_client::Error>(Read {
                    artist: None,
                    shelves: None,
                    revision: loose.revision,
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
                songs: songs_of(loose),
                history: history.ok(),
            })
        });
        self.reading = Some(cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |page, cx| {
                match answer {
                    Ok(Ok(read)) => page.took(read, cx),
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
        }
        let context = PlayContext {
            kind: ContextKind::Artist,
            reference: Some(self.artist_id),
        };
        self.tracks
            .update(cx, |tracks, cx| tracks.set(read.songs, None, context, cx));
        self.problem = false;
    }

    /// Reads a cover at `px` pixels, once, and keeps it.
    fn want(&mut self, art: Id, px: u32, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        if let Some(image) = self.covers.get(&(art, px)) {
            return Some(Arc::clone(image));
        }
        if self.asked.insert((art, px)) {
            self.fetch(art, px, cx, move |page, image, _| {
                page.covers.insert((art, px), image);
            });
        }
        // Meanwhile, as it was read for another size or display.
        self.covers
            .iter()
            .filter(|((at, _), _)| *at == art)
            .max_by_key(|((_, size), _)| *size)
            .map(|(_, image)| Arc::clone(image))
    }

    fn fetch(
        &mut self,
        art: Id,
        px: u32,
        cx: &mut Context<Self>,
        took: impl FnOnce(&mut Self, Arc<RenderImage>, &mut Context<Self>) + 'static,
    ) {
        let http = Arc::clone(&self.net.art);
        let sizes = self.net.art_sizes.clone();
        let read = self
            .net
            .rt()
            .spawn(async move { crate::covers::fetch_square(&http, &sizes, art, px).await });
        self.fetches.push(read.abort_handle());
        cx.spawn(async move |this, cx| {
            if let Ok(Some((image, _, _))) = read.await {
                this.update(cx, |page, cx| {
                    took(page, Arc::new(image), cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
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
        let source = Arc::clone(source);
        let from = Arc::clone(&source);
        let made = self.net.rt().spawn(crate::covers::decode(move || {
            Some(crate::banner::melted(&source, size.0, size.1))
        }));
        self.fetches.push(made.abort_handle());
        cx.spawn(async move |this, cx| {
            let made = made.await.ok().flatten();
            this.update(cx, |page, cx| {
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
            if self.pictured_scale != scale {
                self.sharpen(art, scale, window, cx);
            }
            return;
        }
        self.pictured_for = Some((art, wants_portrait));
        self.pictured_scale = scale;
        self.blur_read += 1;
        let blur_read = self.blur_read;
        // A portrait of another picture, or of none, is not kept meanwhile.
        self.forget_portrait(Some(window), cx);
        self.picture_failed = false;
        let (width, viewport) = viewport_of(window);
        let banner_radius = crate::blur::radius(crate::blur::BANNER.0, width + 120.);
        let wash_radius =
            crate::blur::radius(crate::blur::WASH.0, width.max(viewport + 128.) + 180.);
        let Some(art) = art else {
            self.soft_pictures(banner_radius, wash_radius, Some(window), cx);
            return;
        };
        self.read_face(art, scale, cx);
        self.read_portrait(art, scale, cx);
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
        });
        self.fetches.push(blurs.abort_handle());
        cx.spawn(async move |this, cx| {
            let blurs = blurs.await.ok().flatten();
            this.update(cx, |page, cx| {
                // Too late: the page has asked for its pictures again since.
                if page.blur_read != blur_read {
                    return;
                }
                match blurs {
                    Some((banner, wash)) => {
                        let now = Instant::now();
                        let gone = [
                            crate::blur::replace(&mut page.banner_blur, Arc::new(banner), now),
                            crate::blur::replace(&mut page.blur, Arc::new(wash), now),
                        ];
                        for old in gone.into_iter().flatten() {
                            crate::covers::drop_unshared(old, None, cx);
                        }
                    }
                    // A picture the server cannot make (one in a format it
                    // does not read) is no picture: the page takes its
                    // colour from their name, rather than no colour.
                    None => page.soft_pictures(banner_radius, wash_radius, None, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Moved to a display of another scale: the face read again for it, and
    /// the portrait when the server has it larger. Each shown is kept until
    /// its sharper one comes; the blurs do not depend on the scale.
    fn sharpen(
        &mut self,
        art: Option<Id>,
        scale: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.pictured_scale;
        self.pictured_scale = scale;
        // A soft face, as with no picture or one that cannot be read, is
        // made again for it; a picture's is read again, and replaces it.
        if self.face.as_ref().is_some_and(theme::soft_holds) {
            self.soft_face(Some(window), cx);
        }
        let Some(art) = art else { return };
        self.read_face(art, scale, cx);
        let portrait_px = |scale: f32| self.net.bucket((2. * 380. * scale).ceil() as u32);
        if !self.picture_failed && portrait_px(before) != portrait_px(scale) {
            self.read_portrait(art, scale, cx);
        }
    }

    /// The face that stands in for a picture: soft colour from their name.
    /// A read of a face still to come is let go.
    fn soft_face(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        self.face_read += 1;
        let face = theme::soft_cached(&self.name, (28. * self.pictured_scale).ceil() as u32);
        if let Some(old) = self.face.replace(face) {
            crate::covers::drop_unshared(old, window, cx);
        }
    }

    /// The face for the strip, read for the display's `scale`.
    fn read_face(&mut self, art: Id, scale: f32, cx: &mut Context<Self>) {
        self.face_read += 1;
        let read = self.face_read;
        self.fetch(
            art,
            (28. * scale).ceil() as u32,
            cx,
            move |page, image, cx| {
                // Asked for before the last: for a display the window has
                // left since, or another picture.
                if page.face_read != read {
                    return;
                }
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
            crate::covers::decode(move || crate::covers::decode_picture(&bytes)).await
        });
        self.fetches.push(read.abort_handle());
        cx.spawn(async move |this, cx| {
            let source = read.await.ok().flatten();
            this.update(cx, |page, cx| {
                // Too late: the page shows another picture now, or a later
                // read was asked for, for another display.
                if page.pictured_for.map(|(at, _)| at) != Some(Some(art))
                    || page.portrait_read != asked
                {
                    return;
                }
                let again = page.portrait.is_some();
                match source {
                    Some(source) => {
                        // The same picture read sharper is made again from
                        // this at the next render; the one shown stays until then.
                        if let Some(made) = page.melted.as_mut().filter(|_| again) {
                            made.0 = (0, 0);
                        }
                        page.portrait = Some(Arc::new(source));
                    }
                    None if again => {}
                    None => {
                        page.forget_portrait(None, cx);
                        page.picture_failed = true;
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
    /// this replaces.
    fn soft_pictures(
        &mut self,
        banner_radius: usize,
        wash_radius: usize,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
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
        self.soft_face(window, cx);
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
            .map(|(_, image)| Arc::clone(image));
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
            Some(art) => {
                let read_at = if side > CARD { 220. } else { 140. };
                self.want(art, (read_at * scale).ceil() as u32, cx)
            }
            None => Some(theme::soft_cached(
                &format!("{title}{}", credit.unwrap_or_default()),
                (side * scale).ceil() as u32,
            )),
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
    fn records(&mut self, area: f32, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
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
        let mut out = div().flex().flex_col().gap(px(PART_GAP));
        for line in lines {
            let mut row = div().flex().items_start().gap(px(SHELF_GAP));
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
                let mut cards = div()
                    .w(px(wide))
                    .flex()
                    .flex_wrap()
                    .gap_x(px(gap))
                    .gap_y(px(if first { 26. } else { 22. }));
                let appears = *shelf == Shelf::AppearsOn;
                for (index, tile) in records.iter().enumerate() {
                    cards = cards.child(self.card(
                        (shelf_id(*shelf), index),
                        tile,
                        track,
                        words,
                        appears,
                        window,
                        cx,
                    ));
                }
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
            out = out.child(row);
        }
        out
    }

    /// Your history with them: the picture of your years, and the records
    /// you played most, under it or, `across`, beside it.
    fn history(
        &mut self,
        history: &HistoryOf,
        span: ritornel_contract::YearSpan,
        across: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let color = self.page.as_ref().and_then(|page| page.art_color);
        let picture = record::years(
            format!("You and {}", self.name),
            &history.years,
            span,
            color,
            window,
        );
        if history.records.is_empty() {
            return picture;
        }
        let records = self.played(&history.records, window, cx);
        if across {
            div()
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
                )
        } else {
            picture.child(div().mt(px(14.)).child(records))
        }
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
                    .hover(|style| style.bg(theme::fill()))
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
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.5))
                            .text_color(theme::faint())
                            .font_features(crate::bar::tabular())
                            .child(format!("most in {}", played.peak_year)),
                    ),
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
        let shown_history = pictured(history.as_ref());
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
            let beside = history_beside(content, &counts, shown_history.is_some());
            let across = !beside && content >= HISTORY_ACROSS;
            let column = history_column(content);
            let area = if beside {
                content - HISTORY_GAP - column
            } else {
                content
            };
            let records = self.records(area, window, cx);
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
            let history_part = shown_history
                .map(|(history, span)| self.history(history, span, across && !beside, window, cx));
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
        let head = Rc::new(Cell::new(0f32));
        let start = Rc::clone(&head);
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
