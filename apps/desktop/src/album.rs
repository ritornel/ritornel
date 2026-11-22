//! An album's page (`pages/Album.svelte`): a reading page of 1,400 points,
//! centred, that stays about the album. The banner is the artist's banner
//! to the point (`banner.rs`), with the way back to the artist. The cover
//! is large and hangs out of the banner at the head of the column at the
//! right of the tracks: it is drawn twice, the part in the banner by the
//! banner and the rest by the page, so that the banner can collapse to its
//! strip and the cover scroll away under it. Under the cover, one picture
//! of your years with it; the tracks are at the left, by disc when there is
//! more than one, with the other versions of the record under them. Last,
//! under a rule, one quiet row of the artist's other records.
//!
//! What it reads (`pages/read.ts`): `album.get` and `history.album`, and
//! the artist's records for the row at the foot, read through a view that
//! is closed at once. When the library changes it is read again; after a
//! change of plays alone, only the album and your history with it.
//!
//! GPUI has no blur for an image, so the banner's and the wash's are made
//! from the 128-pixel cover (`blur.rs`); a record with no cover has its
//! soft colours blurred instead, as the real page blurs them.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AppContext, Bounds, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement,
    ObjectFit, ParentElement, Pixels, Render, RenderImage, ScrollEvent, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Subscription, Task, Window, canvas, div, img,
    linear_color_stop, linear_gradient, point, prelude::FluentBuilder, px, size,
};
use ritornel_contract::methods::{AlbumGet, HistoryAlbum, ListClose, ListOpen};
use ritornel_contract::{
    AlbumGetParams, AlbumPage, AlbumRef, AlbumTile, AlbumVersion, ContextKind, Event,
    FILTER_VERSION, Filter, HistoryOf, Id, LibraryChange, ListKind, ListOpenParams, PlayContext,
    PlaySource, Predicate, Quality, Query, Row, Sort, SortKey, ViewParams,
};

use crate::banner::{self, Actions, Banner, Hung};
use crate::link::{Link, LinkEvent};
use crate::net::{Net, NoArt};
use crate::player::Player;
use crate::record::{self, Words};
use crate::theme;
use crate::tracks::Tracks;
use crate::wall::{TileRow, viewport_of};
use crate::words::{self, AlbumColumns, COLUMN_GAP, album_columns, pictured, said};

const MEASURE: f32 = 1400.;
/// How many of the artist's other records are read for the row at the foot.
const MORE: u64 = 40;
/// The row at the foot: its covers, and the least gap between them.
const MORE_CARD: f32 = 116.;
const MORE_GAP: f32 = 16.;

/// What the page asks of the window.
pub(crate) enum AlbumEvent {
    /// Open a record's page: another version, or one of the artist's, its
    /// cover lifted from here when it is shown.
    Open(Arc<TileRow>, Option<record::Lift>),
    /// Up to the record's artist.
    Artist(Id, SharedString),
}

/// Everything the page reads, at once.
struct Read {
    page: AlbumPage,
    history: Option<HistoryOf>,
    more: Option<Vec<AlbumTile>>,
}

/// One of the page's pictures. A reply belongs to the size and cover it was asked for.
#[derive(Default)]
struct PictureRead {
    asked: bool,
    request: u64,
    retries: crate::covers::Retries,
}

impl PictureRead {
    fn reset(&mut self) {
        self.asked = false;
        self.request += 1;
        self.retries = Default::default();
    }

    fn ask(&mut self, art: Id, now: Instant) -> Option<u64> {
        if self.asked || self.retries.waits(art, now) {
            return None;
        }
        self.asked = true;
        self.request += 1;
        Some(self.request)
    }

    fn finish<T>(&mut self, art: Id, result: &Result<T, NoArt>, now: Instant) -> Option<Duration> {
        if matches!(result, Err(NoArt::Failed)) {
            self.asked = false;
            Some(self.retries.failed(art, now))
        } else {
            self.retries.read(art);
            None
        }
    }
}

pub(crate) struct Album {
    net: Arc<Net>,
    player: Entity<Player>,
    album: AlbumRef,
    tracks: Entity<Tracks>,
    /// A song search found, still to be brought into view.
    bring: bool,
    /// The thing whose menu is open, or closing, as the page began to draw.
    menued: Option<(String, f32)>,
    pub row: Arc<TileRow>,
    /// Where the row at the foot has its covers, for them to travel from and back to.
    pub landings: Rc<record::Landings>,
    page: Option<AlbumPage>,
    history: Option<HistoryOf>,
    more: Vec<AlbumTile>,
    more_covers: MoreCovers,
    fetches: crate::covers::FetchBag,
    revision: u64,
    /// Whether it is behind another page, and what it missed there.
    freshness: crate::stage::Freshness,
    /// A read of the whole page asked for and not yet in: a read of plays
    /// alone that overtakes it reads the whole page.
    whole_wanted: bool,
    reading: Option<Task<()>>,
    problem: Option<String>,
    /// The wall's cover, drawn until the large one comes.
    pub small: Option<Arc<RenderImage>>,
    large: Option<(Arc<RenderImage>, Instant)>,
    face: Option<Arc<RenderImage>>,
    /// The display's scale the large cover and the face were read for: on
    /// another display they are read again, sharp there.
    pictured_scale: f32,
    /// The last read of the large cover and of the face asked for: an
    /// earlier one coming late, for another display, is let go.
    large_read: PictureRead,
    face_read: PictureRead,
    blur_read: PictureRead,
    /// The cover blurred for the wash, and for the banner.
    pub blur: crate::blur::Shown,
    banner_blur: crate::blur::Shown,
    pub top: f32,
    /// The stretch past either end.
    elastic: crate::elastic::Elastic,
    /// How tall the page was when last drawn.
    height: Rc<Cell<f32>>,
    /// Whether the cover is somewhere else for now: travelling.
    pub cover_away: bool,
    /// How it shows while it arrives or leaves, as the stage moves it.
    pub look: crate::moves::Look,
    _link: Subscription,
}

impl EventEmitter<AlbumEvent> for Album {}

impl Album {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        net: Arc<Net>,
        link: &Entity<Link>,
        player: Entity<Player>,
        album: AlbumRef,
        row: Arc<TileRow>,
        small: Option<Arc<RenderImage>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // When the library changes, what the page shows is read again, and
        // through a new session when the server is found again.
        let followed = cx.subscribe(link, |page: &mut Self, _, event: &LinkEvent, cx| {
            let plays_only = match event {
                LinkEvent::Connected(net) => {
                    page.reading = None;
                    page.revision = 0;
                    page.net = Arc::clone(net);
                    page.reset_picture_reads();
                    page.more_covers.again();
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
            album,
            tracks: cx.new(|cx| Tracks::new(player.clone(), true, cx)),
            bring: false,
            menued: None,
            row,
            landings: Rc::default(),
            page: None,
            history: None,
            more: Vec::new(),
            more_covers: MoreCovers::default(),
            fetches: crate::covers::FetchBag::default(),
            revision: 0,
            freshness: crate::stage::Freshness::default(),
            whole_wanted: false,
            reading: None,
            problem: None,
            small,
            large: None,
            face: None,
            pictured_scale: 0.,
            large_read: Default::default(),
            face_read: Default::default(),
            blur_read: Default::default(),
            blur: None,
            banner_blur: None,
            top: 0.,
            elastic: crate::elastic::Elastic::default(),
            height: Rc::new(Cell::new(0.)),
            cover_away: false,
            look: crate::moves::Look::default(),
            _link: followed,
        };
        cx.on_release(|page, cx| {
            page.release_images(cx);
        })
        .detach();
        page.read(false, cx);
        page.pictures(window, cx);
        page
    }

    fn reset_picture_reads(&mut self) {
        self.large_read.reset();
        self.face_read.reset();
        self.blur_read.reset();
    }

    /// Drops every picture this page owns from the sprite atlas when the page
    /// is released. `small` is left alone because it belongs to the wall or a
    /// shared soft-colour cache.
    fn release_images(&mut self, cx: &mut gpui::App) {
        self.release_cover(cx);
        for image in self.more_covers.drain() {
            crate::covers::drop_unshared(image, None, cx);
        }
        crate::platform::give_back();
    }

    fn release_cover(&mut self, cx: &mut gpui::App) {
        let images = [
            self.large.take().map(|pair| pair.0),
            self.face.take(),
            self.blur.take().map(|pair| pair.0),
            self.banner_blur.take().map(|pair| pair.0),
        ];
        for image in images.into_iter().flatten() {
            crate::covers::drop_unshared(image, None, cx);
        }
    }

    /// The page shown again, or put behind another: what changed while it
    /// was behind is read.
    pub(crate) fn seen(&mut self, seen: bool, cx: &mut Context<Self>) {
        if let Some(plays_only) = self.freshness.shown(seen) {
            self.read(plays_only, cx);
        }
    }

    /// Reads the page, or after a change of plays alone only the album and
    /// your history with it, keeping the row at the foot as it was.
    fn read(&mut self, plays_only: bool, cx: &mut Context<Self>) {
        let plays_only = plays_only && !self.whole_wanted;
        self.whole_wanted = !plays_only;
        let client = Arc::clone(&self.net.client);
        let album = self.album;
        let read = self.net.rt().spawn(async move {
            let params = AlbumGetParams { album };
            let (page, history) = tokio::join!(
                client.call::<AlbumGet>(&params),
                client.call::<HistoryAlbum>(&params),
            );
            let page = page?;
            let more = if plays_only {
                None
            } else if let Some(artist) = page
                .tile
                .artist_credit
                .as_ref()
                .and_then(|credit| credit.artist_ids.first().copied())
            {
                let opened = client
                    .call::<ListOpen>(&ListOpenParams {
                        kind: ListKind::Albums,
                        query: Query {
                            filter: Some(Filter {
                                version: FILTER_VERSION,
                                predicate: Predicate::ByArtist { artist_id: artist },
                            }),
                            sort: Some(vec![Sort {
                                key: SortKey::Discography,
                            }]),
                            ..Query::default()
                        },
                        window: Some(ritornel_contract::Window {
                            offset: 0,
                            limit: MORE + 1,
                        }),
                        replaces: None,
                    })
                    .await;
                match opened {
                    Ok(opened) => {
                        let _ = client
                            .call::<ListClose>(&ViewParams {
                                view: opened.view.clone(),
                            })
                            .await;
                        let mine = [page.release_group_id, page.release_id];
                        Some(
                            opened
                                .rows
                                .into_iter()
                                .filter_map(|row| match row {
                                    Row::Album(tile) if !mine.contains(&tile.album.id()) => {
                                        Some(tile)
                                    }
                                    _ => None,
                                })
                                .take(MORE as usize)
                                .collect(),
                        )
                    }
                    Err(_) => None,
                }
            } else {
                Some(Vec::new())
            };
            Ok::<_, ritornel_client::Error>(Read {
                page,
                history: history.ok(),
                more,
            })
        });
        self.reading = Some(cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |page, cx| {
                match answer {
                    Ok(Ok(read)) => {
                        // The records at the foot are what a whole read adds:
                        // until they come, one is still wanted.
                        if read.more.is_some() {
                            page.whole_wanted = false;
                        }
                        page.took(read, cx);
                    }
                    Ok(Err(error)) => {
                        if page.page.is_none() {
                            page.problem = Some(error.to_string());
                        }
                    }
                    Err(error) => page.problem = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn took(&mut self, read: Read, cx: &mut Context<Self>) {
        self.revision = self.revision.max(read.page.library_revision).max(
            read.history
                .as_ref()
                .map_or(0, |history| history.library_revision),
        );
        if let Some(history) = read.history {
            self.history = Some(history);
        }
        if let Some(more) = read.more {
            self.more = more;
        }
        let credit = read
            .page
            .tile
            .artist_credit
            .as_ref()
            .map(|credit| credit.text.clone());
        let context = PlayContext {
            kind: ContextKind::Album,
            reference: Some(read.page.release_id),
        };
        let songs = read.page.tracks.clone();
        self.tracks.update(cx, |tracks, cx| {
            tracks.set(songs, read.page.library_revision, credit, context, cx)
        });
        if self.row.art != read.page.tile.art_id {
            self.reset_picture_reads();
            self.release_cover(cx);
            self.small = None;
            self.row = Arc::new(TileRow::of_album(read.page.tile.clone()));
        }
        self.page = Some(read.page);
        self.problem = None;
    }

    /// The covers of the row at the foot, for only the columns that fit.
    fn more_pictures(&mut self, columns: usize, scale: f32, cx: &mut Context<Self>) {
        let px = (MORE_CARD * scale).ceil() as u32;
        self.more_covers.size(px);
        let arts: Vec<Id> = self
            .more
            .iter()
            .take(columns)
            .filter_map(|tile| tile.art_id)
            .collect();
        let now = Instant::now();
        for art in arts {
            if !self.more_covers.ask(art, now) {
                continue;
            }
            self.fetch_opt(art, px, cx, move |page, image, cx| match image {
                Ok(image) => {
                    if let Some(old) = page.more_covers.took(art, px, image) {
                        crate::covers::drop_unshared(old, None, cx);
                    }
                }
                // Not there: it is not asked for again.
                Err(NoArt::Missing) => {}
                Err(NoArt::Failed) => {
                    if let Some(wait) = page.more_covers.failed(art, px, Instant::now()) {
                        crate::covers::notify_after(wait, cx);
                    }
                }
            });
        }
    }

    /// Reads a cover at `px` pixels, and gives it to `took`, or why it
    /// could not be read.
    fn fetch_opt(
        &mut self,
        art: Id,
        px: u32,
        cx: &mut Context<Self>,
        took: impl FnOnce(&mut Self, Result<Arc<RenderImage>, NoArt>, &mut Context<Self>) + 'static,
    ) {
        let http = Arc::clone(&self.net.art);
        let sizes = self.net.art_sizes.clone();
        let read = self
            .net
            .rt()
            .spawn(async move { crate::covers::fetch_square(&http, &sizes, art, px).await });
        self.fetches.push(read.abort_handle());
        cx.spawn(async move |this, cx| {
            let image = read
                .await
                .unwrap_or(Err(NoArt::Failed))
                .map(|(image, _, _)| Arc::new(image));
            this.update(cx, |page, cx| {
                took(page, image, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The large cover, the face for the strip, and the blurs for the
    /// banner and the wash; for a record with no cover, its soft colours.
    fn pictures(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scale = window.scale_factor();
        let scale_changed = self.pictured_scale != scale;
        self.pictured_scale = scale;
        let (width, viewport) = viewport_of(window);
        let banner_radius = crate::blur::radius(crate::blur::BANNER.0, width + 120.);
        let wash_radius =
            crate::blur::radius(crate::blur::WASH.0, width.max(viewport + 128.) + 180.);
        let Some(art) = self.row.art else {
            if self.blur.is_none() {
                self.soft_pictures(banner_radius, wash_radius, Some(window), cx);
            } else if scale_changed {
                self.soft_face(Some(window), cx);
            }
            return;
        };
        if scale_changed {
            self.large_read.reset();
            self.face_read.reset();
            if self.face.as_ref().is_some_and(theme::soft_holds) {
                self.soft_face(Some(window), cx);
            }
        }
        let room = width - crate::upnext::rail(cx);
        let side = album_columns(content_width(room)).cover as f32;
        self.sharp_pictures(art, side, scale, cx);
        let Some(request) = self.blur_read.ask(art, Instant::now()) else {
            return;
        };
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
            let blurs = blurs.await.unwrap_or(Err(NoArt::Failed));
            this.update(cx, |page, cx| {
                if page.blur_read.request != request {
                    return;
                }
                if let Some(wait) = page.blur_read.finish(art, &blurs, Instant::now()) {
                    crate::covers::notify_after(wait, cx);
                }
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
                    }
                    // A cover the server cannot make (one in a format it
                    // does not read) is no cover: the page takes the soft
                    // colours of a record with none, rather than no colour.
                    Err(_) => {
                        if page.blur.is_none() {
                            page.soft_pictures(banner_radius, wash_radius, None, cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The large cover and the face, read for the display's `scale`. Each
    /// shown is kept until the one read for it comes, without fading in again.
    fn sharp_pictures(&mut self, art: Id, side: f32, scale: f32, cx: &mut Context<Self>) {
        let px = (side * scale).ceil() as u32;
        if let Some(request) = self.large_read.ask(art, Instant::now()) {
            self.fetch_opt(art, px, cx, move |page, image, cx| {
                if page.large_read.request != request {
                    return;
                }
                if let Some(wait) = page.large_read.finish(art, &image, Instant::now()) {
                    crate::covers::notify_after(wait, cx);
                }
                if let Ok(image) = image {
                    let at = page
                        .large
                        .as_ref()
                        .map_or_else(Instant::now, |large| large.1);
                    if let Some((old, _)) = page.large.replace((image, at)) {
                        crate::covers::drop_unshared(old, None, cx);
                    }
                }
            });
        }
        let px = (28. * scale).ceil() as u32;
        if let Some(request) = self.face_read.ask(art, Instant::now()) {
            self.fetch_opt(art, px, cx, move |page, image, cx| {
                if page.face_read.request != request {
                    return;
                }
                if let Some(wait) = page.face_read.finish(art, &image, Instant::now()) {
                    crate::covers::notify_after(wait, cx);
                }
                if let Ok(image) = image
                    && let Some(old) = page.face.replace(image)
                {
                    crate::covers::drop_unshared(old, None, cx);
                }
            });
        }
    }

    /// The face of a record with no cover: its soft colours. A read of a
    /// face still to come is let go.
    fn soft_face(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let face = theme::soft_cached(&self.soft_name(), (28. * self.pictured_scale).ceil() as u32);
        if let Some(old) = self.face.replace(face) {
            crate::covers::drop_unshared(old, window, cx);
        }
    }

    /// What a record with no cover takes its soft colours from.
    fn soft_name(&self) -> String {
        self.row
            .no_cover
            .clone()
            .unwrap_or_else(|| format!("{}{}", self.row.title, self.row.artist))
    }

    /// For a record with no cover, the soft colours made from its title and
    /// artist: the face, and the blurs for the banner and the wash.
    /// While the window draws, it is handed `window`, to let go of the face
    /// and the blurs this replaces.
    fn soft_pictures(
        &mut self,
        banner_radius: usize,
        wash_radius: usize,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let name = self.soft_name();
        let soft = theme::soft_pixels(&name, crate::blur::SIDE);
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
        if self.face.is_none() {
            self.soft_face(window, cx);
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (_, viewport) = viewport_of(window);
        let max = (self.height.get() - viewport).max(0.);
        self.elastic
            .scroll(&mut self.top, crate::elastic::Wheel::of(event), max);
        cx.notify();
    }

    /// A click on the `n`th record of the row at the foot, as a script asks.
    pub(crate) fn open_record(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(tile) = self.more.get(n) else { return };
        let row = Arc::new(TileRow::of_album(tile.clone()));
        let image = tile.art_id.and_then(|art| self.more_covers.get(art));
        let lift = record::Lift::of(&self.landings, row.id, image);
        cx.emit(AlbumEvent::Open(row, lift));
    }

    /// Scrolls the page by `dy` at once, as a script's jump does.
    /// Marks the song search found, to be brought to the middle of the page
    /// once it is drawn, when it is not in sight already
    /// (`Tracks.svelte`'s `marked`).
    pub(crate) fn mark(&mut self, song: Id, cx: &mut Context<Self>) {
        self.bring = true;
        self.tracks.update(cx, |tracks, _| tracks.mark = Some(song));
    }

    pub(crate) fn jump(&mut self, dy: f32, viewport: f32) {
        let max = (self.height.get() - viewport).max(0.);
        self.top = (self.top + dy).clamp(0., max);
    }

    /// Why its songs play: as this album.
    fn context(&self) -> Option<PlayContext> {
        let page = self.page.as_ref()?;
        Some(PlayContext {
            kind: ContextKind::Album,
            reference: Some(page.release_id),
        })
    }

    /// Every track in the order shown: what a double-click plays.
    fn shown(&self) -> Vec<Id> {
        self.page.as_ref().map_or_else(Vec::new, |page| {
            page.tracks
                .iter()
                .map(|track| track.appearance_id)
                .collect()
        })
    }

    /// Plays the album from its first track, or shuffled.
    pub(crate) fn play_all(&mut self, shuffle: bool, cx: &mut Context<Self>) {
        let ids = self.shown();
        if ids.is_empty() {
            return;
        }
        let context = self.context();
        self.player.update(cx, |player, cx| {
            player
                .play(PlaySource::Tracks { ids }, None, context, shuffle, cx)
                .detach();
        });
    }

    /// Plays from the track at `at`, and the rest of the list follows.
    pub(crate) fn play_from(&mut self, at: usize, cx: &mut Context<Self>) {
        self.tracks
            .update(cx, |tracks, cx| tracks.play_from(at, cx));
    }

    /// The + of a track: it plays next, and says so for a moment.
    pub(crate) fn play_next(&mut self, at: usize, cx: &mut Context<Self>) {
        self.tracks
            .update(cx, |tracks, cx| tracks.play_next(at, cx));
    }

    /// A click on star `star` of the track at `at`.
    pub(crate) fn rate(&mut self, at: usize, star: u32, cx: &mut Context<Self>) {
        self.tracks
            .update(cx, |tracks, cx| tracks.rate(at, star, cx));
    }

    /// Up to the record's artist.
    pub(crate) fn up_to_artist(&mut self, cx: &mut Context<Self>) {
        let credit = self
            .page
            .as_ref()
            .and_then(|page| page.tile.artist_credit.as_ref());
        if let Some(credit) = credit
            && let Some(artist) = credit.artist_ids.first()
        {
            cx.emit(AlbumEvent::Artist(*artist, credit.text.clone().into()));
        }
    }

    /// Whether it is ready to be seen: what it shows read, and its picture
    /// there (`stage.ready`).
    pub(crate) fn ready(&self) -> bool {
        self.page.is_some()
            && (self.row.art.is_none() || self.small.is_some() || self.blur.is_some())
    }

    /// What its colour is borrowed from: the same again changes nothing.
    pub(crate) fn wash_key(&self) -> String {
        match self.row.art {
            Some(art) => format!("art:{art}"),
            None => format!("record:{}", self.row.id),
        }
    }

    /// Where the large cover is in the page area, for the cover to travel
    /// to, with the page laid out `width` points across: the window's
    /// width, less Up next's rail when the card moves the page over.
    pub(crate) fn cover_bounds(&self, width: f32, origin_y: f32) -> Bounds<Pixels> {
        let gutter = gutter(width);
        let content = content_width(width);
        let AlbumColumns { beside, cover } = album_columns(content);
        let side = cover as f32;
        let top = self.top + self.elastic.shown();
        let (x, y) = if beside {
            (gutter + content - side, 30. - top)
        } else {
            (gutter, banner::HEIGHT + banner::BELOW - top)
        };
        Bounds::new(point(px(x), px(origin_y + y)), size(px(side), px(side)))
    }

    /// The best cover there is now, for the journey back.
    pub(crate) fn best_cover(&self) -> Option<Arc<RenderImage>> {
        self.large
            .as_ref()
            .map(|(image, _)| Arc::clone(image))
            .or_else(|| self.small.clone())
    }

    /// The cover's images as drawn, the large coming up over the small.
    fn cover_images(&self, side: f32, scale: f32) -> (Vec<(Arc<RenderImage>, f32)>, bool) {
        let mut images = Vec::new();
        let mut animating = false;
        if let Some(name) = &self.row.no_cover {
            images.push((theme::soft_cached(name, (side * scale).ceil() as u32), 1.));
            return (images, false);
        }
        if let Some(small) = &self.small {
            images.push((Arc::clone(small), 1.));
        }
        if let Some((large, at)) = &self.large {
            let shown = ease(*at, 200.);
            animating |= shown < 1.;
            images.push((Arc::clone(large), shown));
        }
        (images, animating)
    }

    /// The other versions: one quiet line under the tracks, each a page of its own.
    fn versions(&self, page: &AlbumPage, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let versions: Vec<&AlbumVersion> = page
            .versions
            .iter()
            .filter(|version| version.release_id != page.release_id)
            .collect();
        if versions.is_empty() {
            return None;
        }
        let title = page.tile.title.clone().unwrap_or_default();
        let own = format(page.tile.quality.as_ref());
        // Saying a format tells something only when they differ.
        let differ = versions
            .iter()
            .any(|version| format(version.quality.as_ref()) != own);
        let mut line = div()
            .mt(px(18.))
            .pl(px(14.))
            .flex()
            .flex_wrap()
            .text_size(px(13.))
            .line_height(gpui::relative(1.6))
            .text_color(theme::dim())
            .child(format!(
                "{} in your library:\u{a0}",
                if versions.len() == 1 {
                    "Another version"
                } else {
                    "Other versions"
                }
            ));
        for (at, version) in versions.iter().enumerate() {
            if at > 0 {
                line = line.child(div().text_color(theme::faint()).child("\u{a0}·\u{a0}"));
            }
            let name = version_name(version, &title, differ);
            let tile = AlbumTile {
                album: AlbumRef::Release {
                    id: version.release_id,
                },
                title: version.title.clone(),
                artist_credit: page.tile.artist_credit.clone(),
                year: version.year,
                track_count: version.track_count,
                duration_ms: 0,
                version_count: 0,
                art_id: page.tile.art_id,
                quality: version.quality.clone(),
            };
            let row = Arc::new(TileRow::of_album(tile));
            line = line.child(
                div()
                    .id(("version", at))
                    .text_color(theme::ink())
                    .font_weight(FontWeight(550.))
                    .hover(|style| style.underline())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(AlbumEvent::Open(Arc::clone(&row), None));
                    }))
                    .child(name),
            );
        }
        Some(line)
    }

    /// The artist's other records: one row, smaller and quieter than
    /// anything above it; its heading is the way to all of them.
    fn more(
        &self,
        page: &AlbumPage,
        content: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if self.more.is_empty() {
            return None;
        }
        let artist = page
            .tile
            .artist_credit
            .as_ref()
            .map_or_else(String::new, |credit| credit.text.clone());
        // As many as fit, the room left over spread evenly between them.
        let columns = more_columns(content);
        let gap = if columns > 1 {
            (content - columns as f32 * MORE_CARD) / (columns - 1) as f32
        } else {
            MORE_GAP
        };
        let scale = window.scale_factor();
        let mut row = div().flex().gap(px(gap));
        for (at, tile) in self.more.iter().take(columns).enumerate() {
            let title = tile.title.clone().unwrap_or_else(|| "Untitled".to_owned());
            let name = format!(
                "{title}{}",
                tile.artist_credit
                    .as_ref()
                    .map_or("", |credit| credit.text.as_str())
            );
            let opened = Arc::new(TileRow::of_album(tile.clone()));
            let picture = match tile.art_id {
                Some(art) => self.more_covers.get(art),
                None => Some(theme::soft_cached(&name, (MORE_CARD * scale).ceil() as u32)),
            };
            let landings = Rc::clone(&self.landings);
            let lifted = picture.clone();
            let cover_of_record = picture.clone();
            let card = record::card(
                ("more", at),
                record::Card {
                    title: title.into(),
                    sub: tile.year.map(|year| year.to_string()).unwrap_or_default(),
                    picture,
                    side: MORE_CARD,
                    words: Words {
                        title: 13.,
                        sub: 12.5,
                        weight: 550.,
                    },
                    landing: Some((Rc::clone(&self.landings), opened.id)),
                    menu: crate::menu::strength(
                        self.menued.as_ref(),
                        &crate::menu::record_key(tile.album),
                    ),
                },
                window,
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                let lift = record::Lift::of(&landings, opened.id, lifted.clone());
                cx.emit(AlbumEvent::Open(Arc::clone(&opened), lift));
            }));
            let card = crate::carry::record(card, tile, cover_of_record);
            row = row.child(crate::menu::on_record(card, tile));
        }
        Some(
            div()
                .mt(px(64.))
                .pt(px(26.))
                .border_t_1()
                .border_color(theme::line())
                .child(
                    div()
                        .id("more-by")
                        .mb(px(14.))
                        .flex()
                        .items_baseline()
                        .gap(px(10.))
                        .text_size(px(15.))
                        .font_weight(FontWeight(650.))
                        .text_color(theme::dim())
                        .hover(|style| style.text_color(theme::ink()))
                        .on_click(cx.listener(|page, _, _, cx| page.up_to_artist(cx)))
                        .child(format!("More by {artist}"))
                        .child(div().text_color(theme::faint()).child("›")),
                )
                .child(row),
        )
    }
}

fn gutter(width: f32) -> f32 {
    ((width - MEASURE) / 2.).round().max(40.)
}

fn content_width(width: f32) -> f32 {
    width - 2. * gutter(width)
}

/// How many cards fit across the row at the foot for `content` points.
fn more_columns(content: f32) -> usize {
    (((content + MORE_GAP) / (MORE_CARD + MORE_GAP)).floor() as usize).max(1)
}

/// A format, said briefly: "FLAC 24/96", "MP3 320".
fn format(quality: Option<&Quality>) -> Option<String> {
    let best = &quality?.best;
    let codec = best.codec.to_uppercase();
    Some(if best.lossless {
        let rate = best.sample_rate_hz as f32 / 1000.;
        let rate = if rate.fract() == 0. {
            format!("{rate}")
        } else {
            format!("{rate:.1}")
        };
        match best.bit_depth {
            Some(depth) => format!("{codec} {depth}/{rate}"),
            None => codec,
        }
    } else {
        match best.bitrate_kbps {
            Some(rate) => format!("{codec} {rate}"),
            None => codec,
        }
    })
}

/// How a version is named beside this one: what sets it apart from the
/// record's title, its year, and its format when the formats differ.
fn version_name(version: &AlbumVersion, title: &str, differ: bool) -> String {
    let own = version
        .title
        .clone()
        .unwrap_or_else(|| "Untitled".to_owned());
    let apart = own
        .strip_prefix(&format!("{title} ("))
        .and_then(|rest| rest.strip_suffix(')'))
        .map_or_else(|| own.clone(), str::to_owned);
    let mut parts = vec![apart];
    if let Some(year) = version.year {
        parts.push(year.to_string());
    }
    if differ && let Some(format) = format(version.quality.as_ref()) {
        parts.push(format);
    }
    parts.join(", ")
}

/// How far through `ms` since `start`, eased out.
fn ease(start: Instant, ms: f32) -> f32 {
    let t = (start.elapsed().as_secs_f32() * 1000. / ms).clamp(0., 1.);
    1. - (1. - t).powi(3)
}

impl Render for Album {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (width, viewport) = viewport_of(window);
        // With Up next moving the page over, the page keeps to the room left
        // of the card; the banner still runs to the window's edge.
        let rail = crate::upnext::rail(cx);
        let gutter = gutter(width - rail);
        let content = content_width(width - rail);
        let AlbumColumns { beside, cover } = album_columns(content);
        let side = cover as f32;
        self.pictures(window, cx);
        let max = (self.height.get() - viewport).max(0.);
        self.menued = crate::menu::marked(window, cx);
        if self.bring
            && let Some(row) = self.tracks.read(cx).marked.get()
        {
            self.bring = false;
            // Where the row is in the page's area, which starts under the title bar.
            let y = f32::from(row.origin.y) - crate::wall::TITLE_BAR;
            let height = f32::from(row.size.height);
            if y + height > viewport - 24. {
                self.top += y + height / 2. - viewport / 2.;
            }
        }
        self.top = self.top.clamp(0., max);
        let mut animating = self.elastic.step(Instant::now());
        let top = self.top + self.elastic.shown();
        let scale = window.scale_factor();

        // The cover, the page's part of it.
        let (images, coming) = self.cover_images(side, scale);
        animating |= coming;
        let mut cover_box = div()
            .relative()
            .size(px(side))
            .flex_none()
            .rounded(px(12.))
            .bg(linear_gradient(
                180.,
                linear_color_stop(self.row.soft.0, 0.),
                linear_color_stop(self.row.soft.1, 1.),
            ))
            .shadow(banner::cover_shadow())
            .when(self.cover_away, |cover| cover.invisible())
            .when(!self.cover_away, |cover| {
                cover.children(crate::frost::piece(crate::frost::ground(self.row.soft), 1.))
            });
        // The record's menu, on its own cover, which wears the ring while it is open.
        let ringed =
            crate::menu::strength(self.menued.as_ref(), &format!("cover:{}", self.album.id()));
        if let Some(page) = &self.page {
            let (album, title, credit) = (
                self.album,
                page.tile
                    .title
                    .clone()
                    .unwrap_or_else(|| "Untitled".to_owned()),
                page.tile.artist_credit.clone(),
            );
            cover_box =
                cover_box.on_mouse_down(gpui::MouseButton::Right, move |event, window, cx| {
                    cx.stop_propagation();
                    let entries = crate::menu::record_entries(album, &title, credit.as_ref(), cx);
                    crate::menu::popup(
                        format!("cover:{}", album.id()),
                        entries,
                        event.position,
                        window,
                        cx,
                    );
                });
        }
        for (image, opacity) in images.iter().filter(|_| !self.cover_away) {
            cover_box = cover_box.children(crate::frost::piece(
                crate::frost::Fill::Picture(Arc::clone(image)),
                *opacity,
            ));
        }
        for (image, opacity) in &images {
            cover_box = cover_box.child(
                img(Arc::clone(image))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size(px(side))
                    .rounded(px(12.))
                    .object_fit(ObjectFit::Cover)
                    .opacity(*opacity),
            );
        }
        if ringed > 0. {
            cover_box = cover_box.child(crate::menu::ring(side, 12., ringed));
        }

        let mut body = div()
            .pl(px(gutter))
            .pr(px(gutter + rail))
            .w(px(width))
            .flex()
            .flex_col();
        let page = self.page.clone();
        if let Some(page) = &page {
            let kind = match page.record_type {
                Some(ritornel_contract::RecordType::Single) => "single",
                Some(ritornel_contract::RecordType::Ep) => "EP",
                _ => "album",
            };
            let years = pictured(self.history.as_ref()).map(|(history, span)| {
                record::years(
                    format!("You and this {kind}"),
                    &history.years,
                    span,
                    page.art_color,
                    window,
                )
            });
            let tracks_width = if beside {
                content - side - COLUMN_GAP
            } else {
                content
            };
            // A song carried to Up next shows the record's cover, if it has one.
            let art = self
                .row
                .art
                .and(self.small.clone().or_else(|| self.best_cover()));
            self.tracks.update(cx, |tracks, _| {
                tracks.width = tracks_width.min(900.);
                tracks.columns = 1;
                tracks.art = art;
            });
            let tracks = self.tracks.clone();
            let versions = self.versions(page, cx);
            let left = div()
                .w(px(tracks_width))
                .flex()
                .flex_col()
                .child(tracks)
                .children(versions);
            if beside {
                body = body.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(COLUMN_GAP))
                        .child(left)
                        .child(
                            div()
                                .w(px(side))
                                .mt(px(-(banner::HEIGHT + banner::BELOW - 30.)))
                                .flex()
                                .flex_col()
                                .child(cover_box)
                                .children(years.map(|years| div().mt(px(30.)).child(years))),
                        ),
                );
            } else {
                // Without room, the cover goes under the banner, at the left,
                // with the picture of your years beside it.
                body = body
                    .child(
                        div()
                            .mb(px(30.))
                            .flex()
                            .items_end()
                            .gap(px(32.))
                            .child(cover_box)
                            .children(years.map(|years| div().flex_1().min_w_0().child(years))),
                    )
                    .child(left);
            }
            self.more_pictures(more_columns(content), window.scale_factor(), cx);
            body = body.children(self.more(page, content, window, cx));
        } else {
            body = body.child(
                div()
                    .when(beside, |row| {
                        row.flex()
                            .justify_end()
                            .mt(px(-(banner::HEIGHT + banner::BELOW - 30.)))
                    })
                    .child(cover_box),
            );
            if let Some(problem) = &self.problem {
                body = body.child(
                    div()
                        .text_color(theme::faint())
                        .text_size(px(13.5))
                        .child(format!("This album could not be read: {problem}")),
                );
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

        // The banner, over the page.
        let title: SharedString = page
            .as_ref()
            .and_then(|page| page.tile.title.clone())
            .map_or_else(|| self.row.title.clone(), Into::into);
        let (sentence, faint) = said(self.history.as_ref());
        let hung = beside.then(|| Hung {
            x: gutter + content - side,
            side,
            images: images.clone(),
            ground: self.row.soft,
            // Hidden while a cover flies to it or from it.
            hidden: self.cover_away,
        });
        let up_label: SharedString = page
            .as_ref()
            .and_then(|page| page.tile.artist_credit.as_ref())
            .map_or_else(
                || self.row.artist.clone(),
                |credit| credit.text.clone().into(),
            );
        let banner = Banner {
            title,
            facts: page.as_ref().map(words::album_facts).unwrap_or_default(),
            up_label,
            sentence,
            faint,
            playable: page.as_ref().is_some_and(|page| !page.tracks.is_empty()),
            round: false,
            backdrop: self.banner_blur.clone(),
            color: theme::deep(page.as_ref().and_then(|page| page.art_color)),
            face: self.face.clone(),
            hung,
            portrait: None,
            picture: (1., 1.),
        };
        let actions = Actions {
            up: Rc::new(|page: &mut Self, cx: &mut Context<Self>| page.up_to_artist(cx)),
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
            .id("album")
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

/// The covers of the row at the foot of an album's page, read at one size
/// at a time: the cards' at the display's scale.
#[derive(Default)]
struct MoreCovers {
    covers: HashMap<Id, Arc<RenderImage>>,
    asked: HashSet<(Id, u32)>,
    retries: crate::covers::Retries,
    px: u32,
}

impl MoreCovers {
    /// Reads them at `px` from now on; at another size, each is asked for again.
    fn size(&mut self, px: u32) {
        if self.px != px {
            self.px = px;
            self.asked.clear();
        }
    }

    /// Whether `art` is to be read now: it has not been asked for at this
    /// size, nor is it waiting to be asked for again.
    fn ask(&mut self, art: Id, now: Instant) -> bool {
        !self.retries.waits(art, now) && self.asked.insert((art, self.px))
    }

    /// A cover read at `px` came: kept in place of the one before, which is
    /// handed back to be let go from the sprite atlas. One read for a size
    /// the row has left since, coming after the one for its size now, is not
    /// kept; never drawn, it has nothing in the atlas to let go.
    fn took(&mut self, art: Id, px: u32, image: Arc<RenderImage>) -> Option<Arc<RenderImage>> {
        if px != self.px {
            return None;
        }
        self.retries.read(art);
        self.covers.insert(art, image)
    }

    /// A cover read at `px` could not be read at `now`: it may be asked for
    /// again after the wait given, unless the row has left that size.
    fn failed(&mut self, art: Id, px: u32, now: Instant) -> Option<Duration> {
        self.asked
            .remove(&(art, px))
            .then(|| self.retries.failed(art, now))
    }

    /// A new session: what could not be read is asked for again at once.
    fn again(&mut self) {
        self.retries = crate::covers::Retries::default();
    }

    fn get(&self, art: Id) -> Option<Arc<RenderImage>> {
        self.covers.get(&art).cloned()
    }

    fn drain(&mut self) -> impl Iterator<Item = Arc<RenderImage>> + '_ {
        self.covers.drain().map(|(_, image)| image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> Arc<RenderImage> {
        Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(1, 1),
        )]))
    }

    #[test]
    fn a_picture_failure_retries_and_a_new_cover_invalidates_old_replies() {
        let art = Id::from_bytes([1; 16]);
        let now = Instant::now();
        let mut read = PictureRead::default();
        let first = read.ask(art, now);
        assert!(first.is_some());
        assert!(read.ask(art, now).is_none());
        let Some(wait) = read.finish::<()>(art, &Err(NoArt::Failed), now) else {
            unreachable!()
        };
        assert!(read.ask(art, now).is_none());
        assert!(read.ask(art, now + wait).is_some());
        assert!(
            read.finish::<()>(art, &Err(NoArt::Missing), now + wait)
                .is_none()
        );
        assert!(read.ask(art, now + wait * 2).is_none());
        read.reset();
        assert_ne!(first, Some(read.request));
        assert!(read.ask(art, now).is_some());
    }

    #[test]
    fn a_cover_read_for_a_size_the_row_has_left_is_not_kept() {
        let mut more = MoreCovers::default();
        let art = Id::from_bytes([1; 16]);
        // Asked at one scale, then at another before the first came.
        more.size(100);
        assert!(more.ask(art, Instant::now()));
        more.size(200);
        assert!(more.ask(art, Instant::now()));
        let sharp = tiny();
        assert!(more.took(art, 200, Arc::clone(&sharp)).is_none());
        // The read at the size the row has left comes last.
        assert!(more.took(art, 100, tiny()).is_none());
        assert!(more.get(art).is_some_and(|kept| Arc::ptr_eq(&kept, &sharp)));
    }

    #[test]
    fn a_cover_replaced_at_its_size_is_handed_back_to_be_let_go() {
        let mut more = MoreCovers::default();
        let art = Id::from_bytes([2; 16]);
        more.size(100);
        let first = tiny();
        assert!(more.took(art, 100, Arc::clone(&first)).is_none());
        let gone = more.took(art, 100, tiny());
        assert!(gone.is_some_and(|gone| Arc::ptr_eq(&gone, &first)));
    }

    #[test]
    fn a_cover_that_could_not_be_read_is_asked_for_again_after_a_wait_and_a_missing_one_never() {
        let mut more = MoreCovers::default();
        let (failing, missing) = (Id::from_bytes([3; 16]), Id::from_bytes([4; 16]));
        let now = Instant::now();
        more.size(100);
        assert!(more.ask(failing, now));
        assert!(more.ask(missing, now));
        // Drawn again at once, as a failure draws the page: neither is asked.
        let wait = more.failed(failing, 100, now);
        assert_eq!(wait, Some(Duration::from_secs(2)));
        assert!(!more.ask(failing, now));
        assert!(!more.ask(missing, now));
        // After the wait the one that failed is asked again; the missing
        // one, left asked, is not.
        let later = now + Duration::from_secs(2);
        assert!(more.ask(failing, later));
        assert!(!more.ask(missing, later));
        // A new session asks at once.
        assert!(more.failed(failing, 100, later).is_some());
        more.again();
        assert!(more.ask(failing, later));
        // A failure for a size the row has left asks nothing more, and a
        // read for it does not end the wait at the size it has now.
        more.size(200);
        assert_eq!(more.failed(failing, 100, later), None);
        assert!(more.ask(failing, later));
        assert!(more.failed(failing, 200, later).is_some());
        assert!(more.took(failing, 100, tiny()).is_none());
        assert!(!more.ask(failing, later));
    }

    #[test]
    fn more_columns_matches_visible_row_width() {
        assert_eq!(more_columns(100.), 1);
        assert_eq!(more_columns(content_width(1400.)), 10);
        assert_eq!(more_columns(content_width(3000.)), 10);
    }
}
