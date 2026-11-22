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
use std::time::Instant;

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
use crate::net::Net;
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
    more_covers: HashMap<Id, Arc<RenderImage>>,
    more_asked: HashSet<(Id, u32)>,
    more_px: u32,
    fetches: crate::covers::FetchBag,
    revision: u64,
    reading: Option<Task<()>>,
    problem: Option<String>,
    /// The wall's cover, drawn until the large one comes.
    pub small: Option<Arc<RenderImage>>,
    large: Option<(Arc<RenderImage>, Instant)>,
    face: Option<Arc<RenderImage>>,
    /// The display's scale the large cover and the face were read for: on
    /// another display they are read again, sharp there.
    pictured_scale: f32,
    /// The cover blurred for the wash, and for the banner.
    pub blur: Option<(Arc<RenderImage>, Instant)>,
    banner_blur: Option<(Arc<RenderImage>, Instant)>,
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
        window: &Window,
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
            album,
            tracks: cx.new(|cx| Tracks::new(player.clone(), true, cx)),
            bring: false,
            menued: None,
            row,
            landings: Rc::default(),
            page: None,
            history: None,
            more: Vec::new(),
            more_covers: HashMap::new(),
            more_asked: HashSet::new(),
            more_px: 0,
            fetches: crate::covers::FetchBag::default(),
            revision: 0,
            reading: None,
            problem: None,
            small,
            large: None,
            face: None,
            pictured_scale: 0.,
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

    /// Drops every picture this page owns from the sprite atlas when the page
    /// is released. `small` is left alone because it belongs to the wall or a
    /// shared soft-colour cache.
    fn release_images(&mut self, cx: &mut gpui::App) {
        let mut released = false;
        let mut drop_one = |image: Arc<RenderImage>| {
            released = true;
            crate::covers::drop_unshared(image, None, cx);
        };
        if let Some((image, _)) = self.large.take() {
            drop_one(image);
        }
        if let Some(image) = self.face.take() {
            drop_one(image);
        }
        if let Some((image, _)) = self.blur.take() {
            drop_one(image);
        }
        if let Some((image, _)) = self.banner_blur.take() {
            drop_one(image);
        }
        for (_, image) in self.more_covers.drain() {
            drop_one(image);
        }
        if released {
            crate::platform::give_back();
        }
    }

    /// Reads the page, or after a change of plays alone only the album and
    /// your history with it, keeping the row at the foot as it was.
    fn read(&mut self, plays_only: bool, cx: &mut Context<Self>) {
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
                    Ok(Ok(read)) => page.took(read, cx),
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
        self.tracks
            .update(cx, |tracks, cx| tracks.set(songs, credit, context, cx));
        self.page = Some(read.page);
        self.problem = None;
    }

    /// The covers of the row at the foot, for only the columns that fit.
    fn more_pictures(&mut self, columns: usize, scale: f32, cx: &mut Context<Self>) {
        let px = (MORE_CARD * scale).ceil() as u32;
        if self.more_px != px {
            self.more_px = px;
            self.more_asked.clear();
        }
        let arts: Vec<Id> = self
            .more
            .iter()
            .take(columns)
            .filter_map(|tile| tile.art_id)
            .collect();
        for art in arts {
            if !self.more_asked.insert((art, px)) {
                continue;
            }
            self.fetch_opt(art, px, cx, move |page, image, cx| match image {
                Some(image) => {
                    if let Some(old) = page.more_covers.insert(art, image) {
                        crate::covers::drop_unshared(old, None, cx);
                    }
                }
                None => {
                    page.more_asked.remove(&(art, px));
                }
            });
        }
    }

    /// Reads a cover at `px` pixels, and gives it to `took`, or `None`
    /// when it cannot be read.
    fn fetch_opt(
        &mut self,
        art: Id,
        px: u32,
        cx: &mut Context<Self>,
        took: impl FnOnce(&mut Self, Option<Arc<RenderImage>>, &mut gpui::App) + 'static,
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
                .ok()
                .flatten()
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
    fn pictures(&mut self, window: &Window, cx: &mut Context<Self>) {
        let scale = window.scale_factor();
        self.pictured_scale = scale;
        let (width, viewport) = viewport_of(window);
        let banner_radius = crate::blur::radius(crate::blur::BANNER.0, width + 120.);
        let wash_radius =
            crate::blur::radius(crate::blur::WASH.0, width.max(viewport + 128.) + 180.);
        let Some(art) = self.row.art else {
            self.soft_pictures(banner_radius, wash_radius, scale);
            return;
        };
        let room = width - crate::upnext::rail(cx);
        let side = album_columns(content_width(room)).cover as f32;
        self.sharp_pictures(art, side, scale, cx);
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
                match blurs {
                    Some((banner, wash)) => {
                        let now = Instant::now();
                        page.banner_blur = Some((Arc::new(banner), now));
                        page.blur = Some((Arc::new(wash), now));
                    }
                    // A cover the server cannot make (one in a format it
                    // does not read) is no cover: the page takes the soft
                    // colours of a record with none, rather than no colour.
                    None => page.soft_pictures(banner_radius, wash_radius, scale),
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
        self.fetch_opt(
            art,
            (side * scale).ceil() as u32,
            cx,
            move |page, image, cx| {
                // Read for a display the window has left since.
                let Some(image) = image.filter(|_| page.pictured_scale == scale) else {
                    return;
                };
                let at = page
                    .large
                    .as_ref()
                    .map_or_else(Instant::now, |large| large.1);
                if let Some((old, _)) = page.large.replace((image, at)) {
                    crate::covers::drop_unshared(old, None, cx);
                }
            },
        );
        self.fetch_opt(
            art,
            (28. * scale).ceil() as u32,
            cx,
            move |page, image, cx| {
                let Some(image) = image.filter(|_| page.pictured_scale == scale) else {
                    return;
                };
                if let Some(old) = page.face.replace(image) {
                    crate::covers::drop_unshared(old, None, cx);
                }
            },
        );
    }

    /// Moved to a display of another scale: the large cover and the face read
    /// again for it. The blurs do not depend on the scale.
    fn sharpen(&mut self, side: f32, scale: f32, cx: &mut Context<Self>) {
        self.pictured_scale = scale;
        match self.row.art {
            Some(art) => self.sharp_pictures(art, side, scale, cx),
            None => {
                self.face = Some(theme::soft_cached(
                    &self.soft_name(),
                    (28. * scale).ceil() as u32,
                ))
            }
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
    fn soft_pictures(&mut self, banner_radius: usize, wash_radius: usize, scale: f32) {
        let name = self.soft_name();
        let soft = theme::soft_pixels(&name, crate::blur::SIDE);
        let tone = crate::blur::wash_tone(theme::dark());
        let now = Instant::now();
        self.banner_blur = Some((
            Arc::new(crate::blur::pixels(
                &soft,
                banner_radius,
                crate::blur::BANNER.1,
            )),
            now,
        ));
        self.blur = Some((
            Arc::new(crate::blur::toned(&soft, wash_radius, tone.0, tone.1)),
            now,
        ));
        self.face = Some(theme::soft_cached(&name, (28. * scale).ceil() as u32));
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
        let image = tile
            .art_id
            .and_then(|art| self.more_covers.get(&art).cloned());
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
                Some(art) => self.more_covers.get(&art).cloned(),
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
        if self.pictured_scale != window.scale_factor() {
            self.sharpen(side, window.scale_factor(), cx);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn more_columns_matches_visible_row_width() {
        assert_eq!(more_columns(100.), 1);
        assert_eq!(more_columns(content_width(1400.)), 10);
        assert_eq!(more_columns(content_width(3000.)), 10);
    }
}
