//! The Library walls ("Library walls and cover loading" in
//! docs/internals/desktop.md).
//!
//! The wall keeps its own scroll position and draws only the lines of
//! tiles in view, from scratch each frame (`layout.rs`). Rows are read a
//! page of 100 at a time through a server view; covers come through
//! `covers.rs`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{
    AnyElement, Bounds, Context, EventEmitter, FontWeight, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, Render, ScrollEvent, SharedString,
    StatefulInteractiveElement, Styled, TouchPhase, Window, canvas, div, img, linear_color_stop,
    linear_gradient, point, prelude::FluentBuilder, px, size,
};
use ritornel_contract::methods::LibraryStats;
use ritornel_contract::{
    AlbumRef, Id, ListKind, ListOpenParams, NoParams, Query, Row, ServerRole, Sort, SortKey,
};
use tokio::sync::watch;

use crate::blank::Blank;
use crate::covers::{self, Cache, LoaderCounts, Wants};
use crate::find::{Cut, cut};
use crate::layout::{self, Layout, Shape, TILE};
use crate::link::{Link, LinkEvent, State};
use crate::measure::{Fling, Stats};
use crate::motion::Motion;
use crate::net::Net;
use crate::theme;

/// The page's top margin, heading, and controls row, scrolling away with
/// the wall. The 30-point heading is set on a 1.4 line height (42 pt).
pub(crate) const HEADER: f32 = 20. + 42. + 6. + 52. + 14.;
const FOOT: f32 = 48.;
const PAGE: usize = 100;
/// Pages of rows kept in memory (20,000 rows).
const KEEP_PAGES: usize = 200;
/// How often the wall's view is used while nothing else reads it. The
/// server lets a view go once it has gone ten minutes unused, and tells the
/// wall of no more changes; a wall with its rows all read uses it no more.
const KEEP_VIEW: Duration = Duration::from_secs(4 * 60);
const PAGES_IN_FLIGHT: usize = 4;
/// Decoded covers kept, in bytes of pixels. GPUI's sprite atlas holds a
/// second copy of each on the GPU. `RITORNEL_GPUI_COVER_MB` overrides it,
/// for measuring.
fn cover_budget() -> usize {
    std::env::var("RITORNEL_GPUI_COVER_MB")
        .ok()
        .and_then(|mb| mb.parse::<usize>().ok())
        .unwrap_or(96)
        * 1024
        * 1024
}

/// The tile selected: its row's ID, where the row is, and the view it was read in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selected {
    pub id: Id,
    pub index: usize,
    pub view: String,
}

/// How long after a key the next continues the name.
const TYPING: Duration = Duration::from_millis(1000);

/// What a tile is of.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Subject {
    Album(AlbumRef),
    Artist(Id),
}

/// Which wall the Library shows ("Library walls and cover loading" in
/// docs/internals/desktop.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WallKind {
    Artists,
    Albums,
}

impl WallKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Artists => "artists",
            Self::Albums => "albums",
        }
    }

    /// A count of it: "1 album", "8,653 albums".
    fn count(self, total: usize) -> String {
        let (one, many) = match self {
            Self::Artists => ("artist", "artists"),
            Self::Albums => ("album", "albums"),
        };
        format!(
            "{} {}",
            thousands(total),
            if total == 1 { one } else { many }
        )
    }
}

/// An order of a wall: its key, its name in the menu, and whether it is
/// laid out under headings of time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Order {
    pub key: SortKey,
    pub label: &'static str,
    pub dated: bool,
    /// What the remembered choice calls it.
    pub id: &'static str,
}

/// Each wall's sort orders, the first being the default it opens in.
/// The Artists wall's one order, and the order a wall opens in when none was chosen.
const BY_NAME: Order = Order {
    key: SortKey::Name,
    label: "Name",
    dated: false,
    id: "name",
};
const BY_ARTIST: Order = Order {
    key: SortKey::Artist,
    label: "Artist",
    dated: false,
    id: "artist",
};
const ARTIST_ORDERS: &[Order] = &[Order {
    key: SortKey::Name,
    label: "Name",
    dated: false,
    id: "name",
}];
const ALBUM_ORDERS: &[Order] = &[
    Order {
        key: SortKey::Artist,
        label: "Artist",
        dated: false,
        id: "artist",
    },
    Order {
        key: SortKey::Title,
        label: "Title",
        dated: false,
        id: "title",
    },
    Order {
        key: SortKey::Year,
        label: "Year",
        dated: true,
        id: "year",
    },
    Order {
        key: SortKey::Added,
        label: "Recently added",
        dated: true,
        id: "added",
    },
];

/// An order by the name the remembered choice gives it.
pub(crate) fn order_named(id: &str) -> Option<Order> {
    ALBUM_ORDERS
        .iter()
        .chain(ARTIST_ORDERS)
        .find(|order| order.id == id)
        .copied()
}

/// The order a wall opens in.
fn first_order(kind: WallKind) -> Order {
    match kind {
        WallKind::Artists => BY_NAME,
        WallKind::Albums => BY_ARTIST,
    }
}

fn orders(kind: WallKind) -> &'static [Order] {
    match kind {
        WallKind::Artists => ARTIST_ORDERS,
        WallKind::Albums => ALBUM_ORDERS,
    }
}

pub(crate) struct TileRow {
    pub subject: Subject,
    /// The ID a selection names it by.
    pub id: Id,
    pub title: SharedString,
    /// The line under the title: the artist, or how many albums.
    pub artist: SharedString,
    /// The names type-to-find matches: an artist's name and the one they
    /// are filed under; a record's artist and its title.
    pub names: (Option<String>, Option<String>),
    /// An artist with no picture shows the first letter of their name.
    pub initial: SharedString,
    pub round: bool,
    pub art: Option<Id>,
    pub soft: (Hsla, Hsla),
    /// For a record with no cover: the name its soft colour is made from.
    pub no_cover: Option<String>,
    /// A record's artist, the first its credit names: where Go to Artist goes.
    pub artist_id: Option<Id>,
}

impl TileRow {
    /// A record as a wall's tile has it: what a page it opens is made from.
    pub(crate) fn of_album(tile: ritornel_contract::AlbumTile) -> Self {
        let title = tile.title.unwrap_or_else(|| "Untitled".to_owned());
        let artist_id = tile
            .artist_credit
            .as_ref()
            .and_then(|credit| credit.artist_ids.first().copied());
        let artist = tile
            .artist_credit
            .map_or_else(|| "Unknown artist".to_owned(), |credit| credit.text);
        let name = format!("{title}{artist}");
        let soft = theme::soft(&name);
        let no_cover = tile.art_id.is_none().then(|| name.clone());
        Self {
            subject: Subject::Album(tile.album),
            id: tile.album.id(),
            names: (Some(artist.clone()), Some(title.clone())),
            initial: SharedString::default(),
            round: false,
            title: title.into(),
            artist: artist.into(),
            art: tile.art_id,
            soft,
            no_cover,
            artist_id,
        }
    }
}

enum Page {
    Loading,
    Ready {
        rows: Vec<Option<Arc<TileRow>>>,
        used: u64,
    },
}

/// A tile was clicked: the record, and where its art is in the window.
pub(crate) struct OpenAlbum {
    pub row: Arc<TileRow>,
    pub index: usize,
    pub art: Bounds<Pixels>,
}

impl EventEmitter<OpenAlbum> for Wall {}

/// Go to Artist, from a record's menu.
pub(crate) struct OpenArtist {
    pub artist: Id,
    pub name: SharedString,
}

impl EventEmitter<OpenArtist> for Wall {}

struct HeldView {
    id: String,
    client: Arc<ritornel_client::Client>,
}

impl HeldView {
    async fn open(
        client: &Arc<ritornel_client::Client>,
        params: &ListOpenParams,
    ) -> Result<
        (
            Self,
            ritornel_contract::ListOpened,
            ritornel_contract::ListSections,
        ),
        ritornel_client::Error,
    > {
        let opened = client.list_open(params).await?;
        let held = Self {
            id: opened.view.clone(),
            client: Arc::clone(client),
        };
        let sections = client.list_sections(&held.id).await?;
        Ok((held, opened, sections))
    }
}

impl Drop for HeldView {
    fn drop(&mut self) {
        let id = std::mem::take(&mut self.id);
        let client = Arc::clone(&self.client);
        crate::net::rt().spawn(async move {
            let _ = client.list_close(&id).await;
        });
    }
}

pub(crate) struct Wall {
    link: gpui::Entity<Link>,
    /// The session the wall reads through, while there is one.
    net: Option<Arc<Net>>,
    _link: gpui::Subscription,
    /// Settings › Appearance, followed.
    _looks: gpui::Subscription,
    /// The task that takes covers in for this session.
    arrivals: Option<gpui::Task<()>>,
    /// Uses the view now and then, so that the server keeps it.
    keeping: Option<gpui::Task<()>>,
    /// Whether the view could not be opened.
    failed: bool,
    /// Whether it could not because the hub did not answer, so that it is
    /// opened again once the hub does.
    failed_for_hub: bool,
    view: Option<HeldView>,
    total: usize,
    /// How many songs the library has, read when the wall opens empty:
    /// whether the library is empty, or only the wall.
    songs: Option<u64>,
    sections: Vec<(String, usize)>,
    pages: HashMap<usize, Page>,
    pages_in_flight: usize,
    pub covers: Cache,
    wants: Option<watch::Sender<Arc<Wants>>>,
    pub loader: Arc<Mutex<LoaderCounts>>,
    /// How far the page is scrolled, from its top.
    pub top: f32,
    layout: Option<Layout>,
    pub motion: Motion,
    frame: u64,
    /// The covers in view last frame, for whether one arriving fades.
    in_view: HashSet<Id>,
    /// The rows in view last frame, for counting tiles that came into view unfinished.
    seen: HashSet<usize>,
    pub fling: Option<Fling>,
    pub stats: Stats,
    /// Where the page area starts in the window.
    pub origin_y: f32,
    /// The tile selected, by typing its name or with the arrow keys: a row
    /// of the view, not the focus, so it outlasts scrolling and a visit to
    /// its page.
    pub selected: Option<Selected>,
    /// The tile whose context menu is open, which wears the ring meanwhile.
    pub menu_open: Option<usize>,
    /// What has been typed to find a name, and when the last key was.
    typed: String,
    /// What the lozenge says: what was typed, and the name found cut where
    /// the typing ends, or `None` for nothing found.
    shown: Option<(String, Option<Cut>)>,
    /// Puts the typed name away a second after the last key.
    typing_ends: Option<gpui::Task<()>>,
    /// Bumped with each question, so that only the last key's answer counts.
    asked: u64,
    /// The wall's left margin last frame.
    gutter: f32,
    trim_wake: Option<gpui::Task<()>>,
    /// The tile whose cover is travelling back to it.
    pub away: Option<usize>,
    /// Where the index of letters is open, under the letter that opened it.
    index_at: Option<gpui::Point<Pixels>>,
    /// The page area's height last frame.
    viewport: f32,
    /// The library revision the view was read at.
    revision: u64,
    /// A replacement of the view under way, or waiting its turn, and when
    /// the last began: at most one a second.
    replacing: Option<gpui::Task<()>>,
    last_replace: Option<Instant>,
    replace_again: bool,
    /// After a replacement, the row to keep where it was on screen.
    pending_anchor: Option<(usize, f32)>,
    /// Bumped with each open or replacement of the view, and whenever the
    /// wall or the session changes: an answer to any but the latest is let
    /// go, and the view it opened is closed.
    opening: u64,
    /// The newest library revision a list event has named, of any view: one
    /// newer than a view just opened says it changed while it was opening.
    heard_revision: u64,
    /// The server has let the view go: nothing is read through it until
    /// another takes its place.
    lost: bool,
    /// Which wall, in which order, and whether the large letters stand beside it.
    pub kind: WallKind,
    pub order: Order,
    letters: bool,
    /// The stretch past either end.
    elastic: crate::elastic::Elastic,
    /// Coming up from an artist's page, the one you came from, to scroll
    /// to once the Artists wall is open, and to mark for a moment.
    reveal: Option<Id>,
    locating: Option<gpui::Task<()>>,
    reveal_at: Option<usize>,
    here: Option<(Id, Instant)>,
    /// Back from an artist's page, theirs settles into place.
    settling: Option<(Id, Instant)>,
    /// The player, which the menus play through.
    pub player: gpui::Entity<crate::player::Player>,
}

impl Wall {
    pub(crate) fn new(
        link: gpui::Entity<Link>,
        player: gpui::Entity<crate::player::Player>,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        // 96 MB of pixels: at 2x about 260 covers of 300 pixels square, at
        // 1x four times as many, capped by `covers_capacity`.
        let _ = scale;
        let budget = cover_budget();
        let followed = cx.subscribe(&link, |wall, _, event: &LinkEvent, cx| match event {
            LinkEvent::Connected(net) => wall.start(Arc::clone(net), cx),
            LinkEvent::Restated(net) => wall.restated(Arc::clone(net), cx),
            LinkEvent::Away => wall.end(cx),
            LinkEvent::Server(ritornel_contract::Event::List(event)) => wall.heard(event, cx),
            // A change made while no view was held names no view, but its
            // revision says whether a view that opens meanwhile is older.
            LinkEvent::Server(ritornel_contract::Event::Library(event))
                if event.changed.is_empty()
                    || !event.changed.iter().all(|change| {
                        matches!(
                            change,
                            ritornel_contract::LibraryChange::Stats
                                | ritornel_contract::LibraryChange::Ratings
                        )
                    }) =>
            {
                wall.heard_revision = wall.heard_revision.max(event.library_revision);
            }
            LinkEvent::Server(_) => {}
        });
        // The large letters follow Settings › Appearance at once.
        let looks = cx.observe_global::<crate::remember::Looks>(|wall: &mut Self, cx| {
            let letters = crate::remember::looks(cx).letters;
            if wall.letters != letters {
                wall.letters = letters;
                cx.notify();
            }
        });
        let now = link.read(cx).net();
        let mut wall = Self {
            link,
            player,
            elastic: crate::elastic::Elastic::default(),
            reveal: None,
            locating: None,
            reveal_at: None,
            here: None,
            settling: None,
            net: None,
            _link: followed,
            _looks: looks,
            arrivals: None,
            keeping: None,
            failed: false,
            failed_for_hub: false,
            view: None,
            total: 0,
            songs: None,
            sections: Vec::new(),
            pages: HashMap::new(),
            pages_in_flight: 0,
            covers: Cache::new(budget),
            wants: None,
            loader: Arc::default(),
            top: 0.,
            layout: None,
            motion: Motion::default(),
            frame: 0,
            in_view: HashSet::new(),
            seen: HashSet::new(),
            fling: None,
            stats: Stats::default(),
            origin_y: TITLE_BAR,
            selected: None,
            menu_open: None,
            typed: String::new(),
            shown: None,
            typing_ends: None,
            asked: 0,
            gutter: 40.,
            trim_wake: None,
            away: None,
            index_at: None,
            viewport: 600.,
            revision: 0,
            replacing: None,
            last_replace: None,
            replace_again: false,
            pending_anchor: None,
            opening: 0,
            heard_revision: 0,
            lost: false,
            kind: WallKind::Artists,
            order: BY_NAME,
            letters: true,
        };
        let remembered = crate::remember::load();
        wall.letters = crate::remember::looks(cx).letters;
        wall.kind = if remembered.wall == "albums" {
            WallKind::Albums
        } else {
            WallKind::Artists
        };
        wall.order = orders(wall.kind)
            .iter()
            .find(|order| wall.kind == WallKind::Albums && order.id == remembered.albums_sort)
            .copied()
            .unwrap_or(first_order(wall.kind));
        if let Some(net) = now {
            wall.start(net, cx);
        }
        wall
    }

    /// A session began: the covers are loaded through it, and the view is
    /// opened again. Where the wall was is kept.
    fn start(&mut self, net: Arc<Net>, cx: &mut Context<Self>) {
        let net_holds = net.holds;
        let (wants, mut arrived, loader) = covers::start(Arc::clone(&net));
        self.arrivals = Some(cx.spawn(async move |this, cx| {
            while let Some(first) = arrived.next().await {
                let mut batch = vec![first];
                while let Ok(more) = arrived.try_recv() {
                    batch.push(more);
                }
                let alive = this.update(cx, |wall, cx| {
                    for arrived in batch {
                        let frame = wall.frame;
                        let in_view = &wall.in_view;
                        wall.covers
                            .land(arrived, |art| in_view.contains(&art), frame, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
        self.wants = Some(wants);
        self.loader = loader;
        self.net = Some(net);
        // Revisions are the server's: another may count from elsewhere.
        self.heard_revision = 0;
        self.keeping = Some(cx.spawn(async move |wall, cx| {
            loop {
                cx.background_executor().timer(KEEP_VIEW).await;
                if wall.update(cx, |wall, cx| wall.keep_view(cx)).is_err() {
                    break;
                }
            }
        }));
        self.forget_asks();
        self.view = None;
        self.pages.clear();
        self.pages_in_flight = 0;
        self.failed = false;
        // A server with no library has no wall to open until it has one.
        if net_holds {
            self.open(cx);
        }
        cx.notify();
    }

    /// The server says something new of itself. A library it did not hold
    /// when the session began is opened at once.
    fn restated(&mut self, net: Arc<Net>, cx: &mut Context<Self>) {
        let opens = net.holds && !self.net.as_ref().is_some_and(|held| held.holds);
        // The hub is back: a wall its absence kept from being read is read.
        let back = self.failed && self.failed_for_hub && crate::away::away(&net).is_none();
        if back {
            self.net = Some(net);
            self.try_again(cx);
            return;
        }
        if opens {
            self.start(net, cx);
        } else {
            self.net = Some(net);
            cx.notify();
        }
    }

    /// The session ended: what was read is let go, and the covers already
    /// decoded are kept.
    fn end(&mut self, cx: &mut Context<Self>) {
        self.forget_asks();
        self.keeping = None;
        self.net = None;
        self.wants = None;
        self.arrivals = None;
        self.view = None;
        self.pages.clear();
        self.pages_in_flight = 0;
        cx.notify();
    }

    /// Lets go of every open and replacement under way: their answers are
    /// for a wall, an order or a session since left.
    fn forget_asks(&mut self) {
        self.opening += 1;
        self.lost = false;
        self.replacing = None;
        self.locating = None;
        self.selected = None;
        self.reveal_at = None;
        self.replace_again = false;
        self.pending_anchor = None;
    }

    /// How the wall is laid out: under headings of time for a date, else
    /// under letters in their lane, or under small headings with the large
    /// letters turned off.
    fn shape(&self) -> Shape {
        if self.order.dated {
            Shape::Dated
        } else if self.letters {
            Shape::Lettered
        } else {
            Shape::Headed
        }
    }

    /// Shows another wall or sort order from its top; the choice is
    /// remembered.
    pub(crate) fn choose(&mut self, kind: WallKind, order: Option<Order>, cx: &mut Context<Self>) {
        let order = order.unwrap_or(if kind == self.kind {
            self.order
        } else {
            first_order(kind)
        });
        let order = if kind == WallKind::Albums && order == BY_NAME {
            BY_ARTIST
        } else {
            order
        };
        if (kind, order) == (self.kind, self.order) {
            return;
        }
        self.view = None;
        if kind != WallKind::Artists {
            self.reveal = None;
        }
        self.forget_asks();
        self.kind = kind;
        self.order = order;
        self.pages.clear();
        self.pages_in_flight = 0;
        self.total = 0;
        self.sections.clear();
        self.layout = None;
        self.top = 0.;
        self.elastic.reset();
        self.failed = false;
        let mut remembered = crate::remember::load();
        remembered.wall = kind.name().to_owned();
        if kind == WallKind::Albums {
            remembered.albums_sort = order.id.to_owned();
        }
        crate::remember::save(&remembered);
        self.open(cx);
        cx.notify();
    }

    /// Jumps to the first row at `index`, its line at the top: under a
    /// heading, of time or a letter's small one, leaving room for it.
    pub(crate) fn jump(&mut self, index: usize) {
        let Some(layout) = &self.layout else { return };
        let under = match layout.shape {
            Shape::Dated => 56.,
            Shape::Headed => 36.,
            Shape::Lettered => 0.,
        };
        self.top = (HEADER + layout.y_of(index) - under).max(0.);
        self.elastic.reset();
    }

    /// Up from an artist's page: returns to the Artists wall at that artist,
    /// highlighting their tile briefly.
    pub(crate) fn reveal(&mut self, artist: Id, cx: &mut Context<Self>) {
        if self.kind != WallKind::Artists {
            self.reveal = Some(artist);
            self.choose(WallKind::Artists, None, cx);
        } else {
            self.locate(artist, cx);
        }
    }

    /// Marks an artist's tile for a moment: the one you came up from.
    pub(crate) fn mark(&mut self, artist: Id, cx: &mut Context<Self>) {
        crate::debug(format_args!("wall: marking {artist}"));
        // With Reduce motion, the highlight ring is not drawn at all.
        if crate::platform::reduce_motion() {
            return;
        }
        self.here = Some((artist, Instant::now()));
        cx.notify();
    }

    /// Where a record's or an artist's tile is, among the rows read.
    pub(crate) fn index_of(&self, id: Id) -> Option<usize> {
        self.pages.iter().find_map(|(page, held)| match held {
            Page::Ready { rows, .. } => rows
                .iter()
                .position(|row| row.as_ref().is_some_and(|row| row.id == id))
                .map(|at| page * PAGE + at),
            Page::Loading => None,
        })
    }

    /// Back from an artist's page, their tile settles into place: from a
    /// little larger to its size (`motion.ts`, `settle`).
    pub(crate) fn settle(&mut self, artist: Id, cx: &mut Context<Self>) {
        crate::debug(format_args!("wall: settling {artist}"));
        if crate::platform::reduce_motion() {
            return;
        }
        self.settling = Some((artist, Instant::now()));
        cx.notify();
    }

    /// Finds where an artist is on the wall, and scrolls there.
    fn locate(&mut self, artist: Id, cx: &mut Context<Self>) {
        self.mark(artist, cx);
        let (Some(net), Some(view)) = (self.net.clone(), self.view_id().map(str::to_owned)) else {
            return;
        };
        let client = Arc::clone(&net.client);
        let asked_of = view.clone();
        let located = net.rt().spawn(async move {
            client
                .list_locate(&ritornel_contract::ListLocateParams {
                    view,
                    id: Some(artist.to_string()),
                    prefix: None,
                })
                .await
        });
        self.reveal_at = None;
        self.locating = Some(cx.spawn(async move |wall, cx| {
            let located = located.await;
            crate::debug(format_args!("wall: located {located:?}"));
            if let Ok(Ok(located)) = located
                && let Some(index) = located.index
            {
                wall.update(cx, |wall, cx| {
                    if wall.view_id() != Some(&asked_of) {
                        return;
                    }
                    wall.reveal_at = Some(index as usize);
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// The index of letters, open under the letter that opened it: quiet,
    /// small and faint; a letter with nothing filed under it is faded.
    /// Clicking anywhere else dismisses it.
    fn index(&self, at: gpui::Point<Pixels>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let groups: Vec<(String, usize)> = self
            .layout
            .as_ref()
            .map(|layout| {
                layout
                    .groups
                    .iter()
                    .map(|group| (group.label.clone(), group.start))
                    .collect()
            })
            .unwrap_or_default();
        let veil = div()
            .id("index-veil")
            .absolute()
            .size_full()
            .on_click(cx.listener(|wall, _, _, cx| {
                wall.index_at = None;
                cx.notify();
            }))
            .into_any_element();
        let letters: Vec<String> = ('A'..='Z')
            .map(String::from)
            .chain(std::iter::once("#".to_owned()))
            .collect();
        let buttons = letters.into_iter().map(|letter| {
            let start = groups
                .iter()
                .find(|(label, _)| *label == letter)
                .map(|(_, start)| *start);
            let button = div()
                .id(SharedString::from(format!("index-{letter}")))
                .w(px(26.))
                .h(px(26.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::dim())
                .child(letter);
            match start {
                Some(start) => button
                    .hover(|style| style.bg(theme::fill()).text_color(theme::ink()))
                    .on_click(cx.listener(move |wall, _, _, cx| {
                        wall.index_at = None;
                        wall.jump(start);
                        cx.notify();
                    })),
                None => button.opacity(0.35),
            }
        });
        let panel = div()
            .absolute()
            .left(at.x)
            .top(at.y)
            .p(px(10.))
            .rounded(px(14.))
            .bg(theme::raised())
            .shadow(vec![theme::css_shadow(14., 40., gpui::rgba(0x00000073))])
            .border_1()
            .border_color(theme::line())
            .flex()
            .flex_wrap()
            .gap(px(2.))
            .w(px(9. * 26. + 8. * 2. + 20. + 2.))
            .children(buttons)
            .into_any_element();
        vec![veil, panel]
    }

    /// A key pressed while the Library is on screen: letters and digits find
    /// a name, Backspace takes one back, the arrows move the selection,
    /// Return or ⌘↓ opens it, the menu key opens its menu, and Esc puts the
    /// name away, then the selection. Returns whether the wall consumed it.
    pub(crate) fn key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keys = &event.keystroke;
        let mods = keys.modifiers;
        let plain = !mods.platform && !mods.control && !mods.alt && !mods.function;
        if self.index_at.is_some() && keys.key == "escape" {
            self.index_at = None;
            cx.notify();
            return true;
        }
        match keys.key.as_str() {
            "left" | "right" | "up" | "down" if plain => {
                let step = match keys.key.as_str() {
                    "left" => layout::Step::Left,
                    "right" => layout::Step::Right,
                    "up" => layout::Step::Up,
                    _ => layout::Step::Down,
                };
                self.clear_typed();
                self.step(step, window, cx);
                return true;
            }
            "down" if mods.secondary(cx) => {
                self.open_selected(cx);
                return true;
            }
            "enter" if plain => {
                self.open_selected(cx);
                return true;
            }
            "backspace" if plain && !self.typed.is_empty() => {
                self.typed.pop();
                if self.typed.trim().is_empty() {
                    self.clear_typed();
                } else {
                    self.ask(window, cx);
                }
                cx.notify();
                return true;
            }
            "escape" => {
                if !self.typed.is_empty() || self.shown.is_some() {
                    self.clear_typed();
                } else if self.selected.is_some() {
                    self.selected = None;
                } else {
                    return false;
                }
                cx.notify();
                return true;
            }
            "f10" if mods.shift => {
                self.menu_selected(window, cx);
                return true;
            }
            _ => {}
        }
        // A character of a name: letters and digits, and a space within one.
        let Some(character) = keys.key_char.as_deref().filter(|_| plain) else {
            return false;
        };
        let starts = character.chars().next().is_some_and(char::is_alphanumeric);
        let continues = !self.typed.is_empty() && character == " ";
        if !(starts || continues) || character.chars().count() != 1 {
            return false;
        }
        // Sorted by when added there is no name to find.
        if self.order.key == SortKey::Added {
            return true;
        }
        self.typed.push_str(character);
        self.ask(window, cx);
        cx.notify();
        true
    }

    /// What has been typed, at the foot of the wall: the name found, the
    /// part typed in the ink and the rest quieter, or "No match".
    fn lozenge(&self, viewport: f32, rail: f32) -> Option<AnyElement> {
        let (typed, found) = self.shown.as_ref()?;
        let (text, highlights) = match found {
            Some(found) => {
                let text = format!("{}{}{}", found.before, found.typed, found.after);
                let quiet = gpui::HighlightStyle {
                    color: Some(theme::faint().into()),
                    ..Default::default()
                };
                let before = found.before.len();
                let after = before + found.typed.len();
                (
                    text.clone(),
                    vec![(0..before, quiet), (after..text.len(), quiet)],
                )
            }
            None => (
                typed.clone(),
                vec![(
                    0..typed.len(),
                    gpui::HighlightStyle {
                        color: Some(theme::dim().into()),
                        ..Default::default()
                    },
                )],
            ),
        };
        let lozenge = div()
            .max_w(px(520.))
            .px(px(18.))
            .py(px(9.))
            .rounded_full()
            .bg(theme::raised())
            .shadow(vec![theme::css_shadow(14., 40., gpui::rgba(0x00000073))])
            .border_1()
            .border_color(theme::line())
            .flex()
            .items_baseline()
            .text_size(px(15.))
            .line_height(px(20.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::ink())
            .whitespace_nowrap()
            .overflow_hidden()
            .child(
                gpui::StyledText::new(text).with_highlights(
                    highlights
                        .into_iter()
                        .filter(|(range, _)| !range.is_empty()),
                ),
            )
            .when(found.is_none(), |lozenge| {
                lozenge.child(
                    div()
                        .ml(px(10.))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::faint())
                        .child("No match"),
                )
            });
        Some(
            div()
                .absolute()
                .left_0()
                .right(px(rail))
                .top(px(viewport - 22. - 40.))
                .flex()
                .justify_center()
                .child(lozenge)
                .into_any_element(),
        )
    }

    /// Puts the typed name away; the selection stays.
    fn clear_typed(&mut self) {
        self.asked += 1;
        self.typed.clear();
        self.shown = None;
        self.typing_ends = None;
    }

    /// Asks the server where the name typed is filed, and goes there.
    fn ask(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.typing_ends = Some(cx.spawn(async move |wall, cx| {
            cx.background_executor().timer(TYPING).await;
            let _ = wall.update(cx, |wall, cx| {
                wall.clear_typed();
                cx.notify();
            });
        }));
        let (Some(net), Some(view)) = (self.net.clone(), self.view_id().map(str::to_owned)) else {
            return;
        };
        self.asked += 1;
        let asked = self.asked;
        let typed = self.typed.clone();
        let client = Arc::clone(&net.client);
        let (located_view, prefix) = (view.clone(), typed.clone());
        let located = net.rt().spawn(async move {
            let located = client
                .call::<ritornel_contract::methods::ListLocate>(
                    &ritornel_contract::ListLocateParams {
                        view: located_view.clone(),
                        id: None,
                        prefix: Some(prefix),
                    },
                )
                .await?;
            let Some(index) = located.index else {
                return Ok(None);
            };
            let rows = client.list_window(&located_view, index, 1).await?.rows;
            Ok::<_, ritornel_client::Error>(Some((index as usize, rows)))
        });
        cx.spawn(async move |wall, cx| {
            let answer = located.await;
            let _ = wall.update(cx, |wall, cx| {
                if asked != wall.asked || wall.view_id() != Some(view.as_str()) {
                    return;
                }
                // A view that has just been replaced: the next key finds it.
                let Ok(Ok(found)) = answer else { return };
                let Some((index, rows)) = found else {
                    wall.shown = Some((typed, None));
                    cx.notify();
                    return;
                };
                let Some(row) = rows_of(rows).into_iter().flatten().next() else {
                    return;
                };
                let names: Vec<String> = if wall.order.key == SortKey::Year {
                    wall.sections
                        .iter()
                        .find(|(_, start)| *start == index)
                        .map(|(label, _)| label.clone())
                        .into_iter()
                        .collect()
                } else if row.round {
                    [row.names.0.clone(), row.names.1.clone()]
                        .into_iter()
                        .flatten()
                        .collect()
                } else if wall.order.key == SortKey::Title {
                    row.names.1.clone().into_iter().collect()
                } else {
                    row.names.0.clone().into_iter().collect()
                };
                let Some(found) = names.iter().find_map(|name| cut(name, &typed)) else {
                    wall.shown = Some((typed, None));
                    cx.notify();
                    return;
                };
                wall.shown = Some((typed, Some(found)));
                wall.selected = Some(Selected {
                    id: row.id,
                    index,
                    view: view.clone(),
                });
                wall.jump(index);
                cx.notify();
            });
        })
        .detach();
    }

    /// Moves the selection a step, or selects the first tile in view when
    /// there is none, scrolling only as far as keeps it in view.
    fn step(&mut self, step: layout::Step, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(layout), Some(view)) = (self.layout.clone(), self.view_id().map(str::to_owned))
        else {
            return;
        };
        let viewport = viewport_of(window).1;
        let wall_top = self.top - HEADER;
        let index = match &self.selected {
            // Read in a view since replaced: it is found again in this one.
            Some(selected) if selected.view != view => return,
            Some(selected) => layout.step_from(selected.index, step),
            None => match layout.first_in_view(wall_top, wall_top + viewport) {
                Some(index) => index,
                None => return,
            },
        };
        self.keep_in_view(index, viewport);
        if self.selected.as_ref().map(|selected| selected.index) != Some(index)
            && let Some(row) = self.row(index)
        {
            self.selected = Some(Selected {
                id: row.id,
                index,
                view,
            });
        }
        cx.notify();
    }

    /// Scrolls only as far as brings the line of row `index` into view, a
    /// gap clear of the edges; up to the wall's first line brings the page's
    /// top with it.
    fn keep_in_view(&mut self, index: usize, viewport: f32) {
        let Some(layout) = &self.layout else { return };
        let wall_top = self.top - HEADER;
        if let Some(next) = layout.scroll_to_show(index, wall_top, viewport, 0., layout::GAP_Y) {
            self.elastic.reset();
            self.top = if next < wall_top && layout.y_of(index) == layout.y_of(0) {
                0.
            } else {
                (HEADER + next).max(0.)
            };
        }
    }

    /// The selection, brought into view: its row, and where its art is.
    fn reach(&mut self, viewport: f32) -> Option<(Arc<TileRow>, usize, Bounds<Pixels>)> {
        let selected = self.selected.clone()?;
        if self.view_id() != Some(selected.view.as_str()) {
            return None;
        }
        let row = self
            .row(selected.index)
            .filter(|row| row.id == selected.id)?;
        self.keep_in_view(selected.index, viewport);
        let art = self.art_bounds(selected.index)?;
        Some((row, selected.index, art))
    }

    /// Opens the selection, as a click on it does, its cover travelling.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let viewport = self.viewport;
        if let Some((row, index, art)) = self.reach(viewport) {
            cx.emit(OpenAlbum { row, index, art });
        }
    }

    /// Opens the selection's context menu, under it.
    fn menu_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = viewport_of(window).1;
        if let Some((row, index, art)) = self.reach(viewport) {
            self.menu_open = Some(index);
            cx.notify();
            let at = gpui::point(art.origin.x + px(20.), art.origin.y + art.size.height);
            crate::menu::record_menu(row, at, window, cx);
        }
    }

    /// Called when the server says the view may be out of date: a change
    /// that leaves the order as it was is read again in place, the old rows
    /// staying until the new ones come; one that may reorder it is answered
    /// by a view that replaces this one, at most once a second, keeping the
    /// row at the top where it was on screen.
    fn heard(&mut self, event: &ritornel_contract::ListEvent, cx: &mut Context<Self>) {
        self.heard_revision = self.heard_revision.max(event.library_revision);
        if self.view_id() != Some(event.view.as_str()) || event.library_revision <= self.revision {
            return;
        }
        crate::debug(format_args!(
            "list: revision {}, reorder {}",
            event.library_revision, event.reorder
        ));
        if event.reorder {
            self.schedule_replace(cx);
            return;
        }
        self.revision = event.library_revision;
        let Some(layout) = &self.layout else { return };
        let wall_top = self.top - HEADER;
        let lines = layout.lines_between(wall_top - self.viewport, wall_top + 2. * self.viewport);
        let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
            return;
        };
        let pages: Vec<usize> = (first.first / PAGE..=(last.first + last.count) / PAGE).collect();
        for page in pages {
            if matches!(self.pages.get(&page), Some(Page::Ready { .. })) {
                self.read_again(page, cx);
            }
        }
    }

    /// Reads a page again in place: what is on screen stays until it comes.
    fn read_again(&mut self, page: usize, cx: &mut Context<Self>) {
        let (Some(view), Some(net)) = (self.view_id().map(str::to_owned), self.net.clone()) else {
            return;
        };
        let client = Arc::clone(&net.client);
        let asked_of = view.clone();
        let read = net.rt().spawn(async move {
            client
                .list_window(&view, (page * PAGE) as u64, PAGE as u64)
                .await
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(window)) = read.await else { return };
            let _ = this.update(cx, |wall, cx| {
                if wall.view_id() == Some(asked_of.as_str()) {
                    let frame = wall.frame;
                    wall.pages.insert(
                        page,
                        Page::Ready {
                            rows: rows_of(window.rows),
                            used: frame,
                        },
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn schedule_replace(&mut self, cx: &mut Context<Self>) {
        if self.replacing.is_some() {
            self.replace_again = true;
            return;
        }
        let wait = self.last_replace.map_or(Duration::ZERO, |last| {
            Duration::from_secs(1).saturating_sub(last.elapsed())
        });
        self.replacing = Some(cx.spawn(async move |wall, cx| {
            cx.background_executor().timer(wait).await;
            let _ = wall.update(cx, |wall, cx| wall.replace(cx));
        }));
    }

    /// Opens a view that replaces this one, naming the row at the top to
    /// keep in place; everything changes at once when its rows are in.
    fn replace(&mut self, cx: &mut Context<Self>) {
        self.last_replace = Some(Instant::now());
        let (Some(net), Some(old)) = (self.net.clone(), self.view_id().map(str::to_owned)) else {
            self.replacing = None;
            return;
        };
        // Not laid out yet, empty or never drawn, there is no row to keep in
        // place: the view is opened afresh.
        let Some(layout) = self.layout.clone() else {
            self.forget_asks();
            self.view = None;
            self.pages.clear();
            self.pages_in_flight = 0;
            self.open(cx);
            return;
        };
        let wall_top = (self.top - HEADER).max(0.);
        let index = layout.index_at(wall_top);
        let anchor = self.row(index);
        let screen = layout.y_of(index) - wall_top;
        let client = Arc::clone(&net.client);
        let kind = match self.kind {
            WallKind::Artists => ListKind::Artists,
            WallKind::Albums => ListKind::Albums,
        };
        let key = self.order.key;
        self.opening += 1;
        let asked = self.opening;
        let selected = self.selected.clone();
        let selected_id = selected.as_ref().map(|selected| selected.id);
        let offset = (index / PAGE * PAGE) as u64;
        let replaced = net.rt().spawn(async move {
            let (held, opened, sections) = HeldView::open(
                &client,
                &ListOpenParams {
                    kind,
                    query: Query {
                        sort: Some(vec![Sort { key }]),
                        ..Query::default()
                    },
                    window: Some(ritornel_contract::Window {
                        offset,
                        limit: PAGE as u64,
                    }),
                    replaces: Some(ritornel_contract::Replaces {
                        view: old,
                        anchor: anchor.as_ref().map(|row| row.id.to_string()),
                    }),
                },
            )
            .await?;
            // The rows around the anchor, so that what replaces the old ones is there at once.
            let near = match opened.anchor_index {
                Some(at) if at / PAGE as u64 * PAGE as u64 != offset => Some(
                    client
                        .list_window(&opened.view, at / PAGE as u64 * PAGE as u64, PAGE as u64)
                        .await?,
                ),
                _ => None,
            };
            // The selection, found again in the new view.
            let found = match &selected {
                Some(selected) => client
                    .call::<ritornel_contract::methods::ListLocate>(
                        &ritornel_contract::ListLocateParams {
                            view: opened.view.clone(),
                            id: Some(selected.id.to_string()),
                            prefix: None,
                        },
                    )
                    .await
                    .ok()
                    .and_then(|located| located.index),
                None => None,
            };
            let songs = songs_if_empty(&client, opened.total).await;
            Ok::<_, ritornel_client::Error>((held, opened, sections, near, found, songs))
        });
        cx.spawn(async move |wall, cx| {
            let answer = replaced.await;
            let _ = wall.update(cx, |wall, cx| {
                // Asked for a wall, an order or a session since left.
                if wall.opening != asked {
                    return;
                }
                wall.replacing = None;
                if let Ok(Ok((held, opened, sections, near, found, songs))) = answer {
                    wall.total = opened.total as usize;
                    wall.songs = songs;
                    wall.sections = sections
                        .sections
                        .into_iter()
                        .map(|section| (section.label, section.index as usize))
                        .collect();
                    wall.pages.clear();
                    wall.pages_in_flight = 0;
                    // A list grown shorter than the rows asked for answers from
                    // its end, not from the start of a page: those are read anew.
                    if opened.offset == offset {
                        wall.pages.insert(
                            opened.offset as usize / PAGE,
                            Page::Ready {
                                rows: rows_of(opened.rows),
                                used: wall.frame,
                            },
                        );
                    }
                    if let Some(near) = near {
                        wall.pages.insert(
                            near.offset as usize / PAGE,
                            Page::Ready {
                                rows: rows_of(near.rows),
                                used: wall.frame,
                            },
                        );
                    }
                    if let Some(at) = opened.anchor_index {
                        wall.pending_anchor = Some((at as usize, screen));
                    }
                    // Found again for the row selected when asked, not one
                    // chosen since, whose place is in the old view.
                    wall.selected = match (wall.selected.take(), found) {
                        (Some(selected), Some(index)) if Some(selected.id) == selected_id => {
                            Some(Selected {
                                index: index as usize,
                                view: opened.view.clone(),
                                ..selected
                            })
                        }
                        _ => None,
                    };
                    crate::debug(format_args!(
                        "list: replaced, anchor at {:?}",
                        opened.anchor_index
                    ));
                    wall.revision = opened.library_revision;
                    wall.reveal_at = None;
                    wall.view = Some(held);
                    wall.lost = false;
                    wall.layout = None;
                    // The library changed while it was opening.
                    if wall.heard_revision > wall.revision {
                        wall.replace_again = true;
                    }
                } else {
                    // Not opened or not read: asked again, a second on at the soonest.
                    wall.replace_again = true;
                }
                if std::mem::take(&mut wall.replace_again) {
                    wall.schedule_replace(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Uses the view, reading its first row, so that the server keeps it; one
    /// the server has let go already is opened again in its place.
    fn keep_view(&mut self, cx: &mut Context<Self>) {
        let (Some(view), Some(net)) = (self.view_id().map(str::to_owned), self.net.clone()) else {
            return;
        };
        let asked_of = view.clone();
        let client = Arc::clone(&net.client);
        let read = net
            .rt()
            .spawn(async move { client.list_window(&view, 0, 1).await });
        cx.spawn(async move |wall, cx| {
            let answer = read.await;
            wall.update(cx, |wall, cx| {
                if wall.view_id() == Some(asked_of.as_str()) && expired(&answer) {
                    wall.lost = true;
                    if wall.replacing.is_none() {
                        wall.schedule_replace(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn view_id(&self) -> Option<&str> {
        self.view.as_ref().map(|held| held.id.as_str())
    }

    /// Opens the view again in a moment, if it has still not been read.
    fn try_again_soon(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            this.update(cx, |wall, cx| {
                if wall.failed {
                    wall.try_again(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Opens the view again, as "Try again" does.
    pub(crate) fn try_again(&mut self, cx: &mut Context<Self>) {
        self.failed = false;
        self.open(cx);
        cx.notify();
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.net.clone() else { return };
        let client = Arc::clone(&net.client);
        let kind = match self.kind {
            WallKind::Artists => ListKind::Artists,
            WallKind::Albums => ListKind::Albums,
        };
        let key = self.order.key;
        self.opening += 1;
        let asked = self.opening;
        let opened = net.rt().spawn(async move {
            let (held, opened, sections) = HeldView::open(
                &client,
                &ListOpenParams {
                    kind,
                    query: Query {
                        sort: Some(vec![Sort { key }]),
                        ..Query::default()
                    },
                    window: Some(ritornel_contract::Window {
                        offset: 0,
                        limit: PAGE as u64,
                    }),
                    replaces: None,
                },
            )
            .await?;
            let songs = songs_if_empty(&client, opened.total).await;
            Ok::<_, ritornel_client::Error>((held, opened, sections, songs))
        });
        cx.spawn(async move |this, cx| {
            let answer = opened.await;
            let for_hub = hub_unreachable(&answer);
            this.update(cx, |wall, cx| {
                // Opened for a wall, an order or a session since left, or
                // overtaken by another open.
                if wall.opening != asked {
                    return;
                }
                match answer {
                    Ok(Ok((held, opened, sections, songs))) => {
                        wall.total = opened.total as usize;
                        wall.songs = songs;
                        wall.sections = sections
                            .sections
                            .into_iter()
                            .map(|section| (section.label, section.index as usize))
                            .collect();
                        wall.pages.insert(
                            0,
                            Page::Ready {
                                rows: rows_of(opened.rows),
                                used: 0,
                            },
                        );
                        wall.revision = opened.library_revision;
                        wall.reveal_at = None;
                        wall.view = Some(held);
                        wall.lost = false;
                        wall.layout = None;
                        // The library changed while it was opening.
                        if wall.heard_revision > wall.revision {
                            wall.schedule_replace(cx);
                        }
                        if let Some(artist) = wall.reveal.take() {
                            wall.locate(artist, cx);
                        }
                    }
                    Ok(Err(_)) | Err(_) => {
                        wall.failed = true;
                        wall.failed_for_hub = for_hub;
                        // The hub came back before this answer did: no
                        // word of it is coming, so the wall asks again in
                        // a moment.
                        if for_hub
                            && wall
                                .net
                                .as_deref()
                                .is_some_and(|net| crate::away::away(net).is_none())
                        {
                            wall.try_again_soon(cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn row(&mut self, index: usize) -> Option<Arc<TileRow>> {
        match self.pages.get_mut(&(index / PAGE)) {
            Some(Page::Ready { rows, used }) => {
                *used = self.frame;
                rows.get(index % PAGE).cloned().flatten()
            }
            _ => None,
        }
    }

    fn fetch_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let (Some(view), Some(net)) = (self.view_id().map(str::to_owned), self.net.clone()) else {
            return;
        };
        if self.lost {
            return;
        }
        self.pages.insert(page, Page::Loading);
        self.pages_in_flight += 1;
        let asked_of = view.clone();
        let client = Arc::clone(&net.client);
        let read = net.rt().spawn(async move {
            client
                .list_window(&view, (page * PAGE) as u64, PAGE as u64)
                .await
        });
        cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |wall, cx| {
                // An answer for a view since closed, with its session.
                if wall.view_id() != Some(asked_of.as_str()) {
                    return;
                }
                wall.pages_in_flight = wall.pages_in_flight.saturating_sub(1);
                match answer {
                    Ok(Ok(window)) => {
                        let frame = wall.frame;
                        wall.pages.insert(
                            page,
                            Page::Ready {
                                rows: rows_of(window.rows),
                                used: frame,
                            },
                        );
                        wall.evict_pages();
                    }
                    // Unused for long, the view was let go by the server: one
                    // opened in its place keeps the row at the top where it is.
                    answer => {
                        wall.pages.remove(&page);
                        if expired(&answer) {
                            wall.lost = true;
                            if wall.replacing.is_none() {
                                wall.schedule_replace(cx);
                            }
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn evict_pages(&mut self) {
        if self.pages.len() <= KEEP_PAGES {
            return;
        }
        let mut old: Vec<(u64, usize)> = self
            .pages
            .iter()
            .filter_map(|(page, held)| match held {
                Page::Ready { used, .. } => Some((*used, *page)),
                Page::Loading => None,
            })
            .collect();
        old.sort_unstable();
        let extra = self.pages.len() - KEEP_PAGES;
        for (_, page) in old.into_iter().take(extra) {
            self.pages.remove(&page);
        }
    }

    /// The height of the page: header, wall and foot.
    fn content_height(&self) -> f32 {
        HEADER + self.layout.as_ref().map_or(0., |layout| layout.height) + FOOT
    }

    /// Moves the page `dy` points down, as a scroll event or the measure's fling does.
    /// Scrolls the wall as WebKit scrolls a page: a touchpad's scroll may
    /// stretch it past either end.
    pub(crate) fn scroll_by(
        &mut self,
        wheel: crate::elastic::Wheel,
        phase: TouchPhase,
        viewport: f32,
        now: Instant,
    ) {
        let max = (self.content_height() - viewport).max(0.);
        let before = self.top;
        self.elastic.scroll(&mut self.top, wheel, max);
        self.motion.event(self.top - before, phase, now);
    }

    /// What is wanted, most wanted first: where the flick will rest, what
    /// is in view, the way between, and a screen past it.
    fn regions(&self, viewport: f32, now: Instant) -> Vec<(f32, f32)> {
        let max = (self.content_height() - viewport).max(0.);
        let velocity = self.motion.velocity(now);
        let throw = self.motion.throw(now);
        let rest = (self.top + throw).clamp(0., max);
        let mut regions = Vec::new();
        if velocity.abs() > 600. && (rest - self.top).abs() > viewport * 0.5 {
            regions.push((rest, rest + viewport));
        }
        // What is in view, leading edge first, as it stays longest; but not
        // above 6,000 points a second, when a screen passes in a sixth of
        // one. (Measured both ways under the fling: no difference beyond
        // the noise, so the plainer rule.)
        if velocity.abs() <= 6000. || regions.is_empty() {
            let half = self.top + viewport * 0.5;
            if velocity < 0. {
                regions.push((self.top, half));
                regions.push((half, self.top + viewport));
            } else {
                regions.push((half, self.top + viewport));
                regions.push((self.top, half));
            }
        }
        let direction = if velocity < 0. { -1. } else { 1. };
        if rest != self.top {
            let (from, to) = if rest > self.top {
                (self.top + viewport, rest)
            } else {
                (rest + viewport, self.top)
            };
            regions.push((from, to));
        }
        // A screen on in the direction of travel, and half one behind.
        let ahead = if direction > 0. {
            rest + viewport
        } else {
            rest - viewport
        };
        regions.push((
            ahead.min(rest),
            ahead.max(rest) + if direction > 0. { viewport } else { 0. },
        ));
        let behind = if direction > 0. {
            self.top - viewport * 0.5
        } else {
            self.top + viewport
        };
        regions.push((behind, behind + viewport * 0.5));
        regions
    }

    fn plan(&mut self, viewport: f32, px: u32, now: Instant, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.clone() else {
            return;
        };
        let regions = self.regions(viewport, now);
        let capacity = (self.covers_capacity(px) * 4 / 5).max(32);
        let mut ids = Vec::new();
        let mut wanted = HashSet::new();
        let mut pages = Vec::new();
        for (from, to) in regions {
            let lines = layout.lines_between(from - HEADER, to - HEADER);
            // Nearest the start of the region first, in the direction it is read.
            for line in &lines {
                for page in [line.first / PAGE, (line.first + line.count - 1) / PAGE] {
                    if !pages.contains(&page) {
                        pages.push(page);
                    }
                }
                for index in line.first..line.first + line.count {
                    if ids.len() >= capacity {
                        break;
                    }
                    let Some(row) = self.row(index) else { continue };
                    let Some(art) = row.art else { continue };
                    if self.covers.has(art, px)
                        || self.covers.missing.contains(&art)
                        || !wanted.insert(art)
                    {
                        continue;
                    }
                    ids.push(art);
                }
            }
        }
        // Then every other page, while there is nothing nearer to read:
        // rows are small (8,600 are about 2 MB), and a tile whose row is
        // read can show its colour the moment it is in view.
        if self.pages.len() < KEEP_PAGES {
            let all = self.total.div_ceil(PAGE).min(KEEP_PAGES);
            pages.extend(
                (0..all)
                    .filter(|page| !self.pages.contains_key(page))
                    .take(PAGES_IN_FLIGHT),
            );
        }
        for page in pages {
            if self.pages_in_flight >= PAGES_IN_FLIGHT {
                break;
            }
            if page * PAGE < self.total && !self.pages.contains_key(&page) {
                self.fetch_page(page, cx);
            }
        }
        let wants = Wants { ids, px };
        if let Some(sender) = &self.wants
            && **sender.borrow() != wants
        {
            sender.send_replace(Arc::new(wants));
        }
    }

    fn covers_capacity(&self, px: u32) -> usize {
        let each = (px as usize).pow(2) * 4;
        (cover_budget() / each.max(1)).clamp(64, 1200)
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.fling.is_some() {
            return;
        }
        let viewport = viewport_of(window).1;
        let wheel = crate::elastic::Wheel::of(event);
        crate::debug(format_args!(
            "scroll: {:.1} (unaccelerated {:.1}), {:?}/{:?}, stretch {}",
            wheel.dy,
            wheel.unaccelerated_dy,
            wheel.phase,
            wheel.momentum,
            self.elastic.shown()
        ));
        self.scroll_by(wheel, event.touch_phase, viewport, Instant::now());
        cx.notify();
    }
}

fn rows_of(rows: Vec<Row>) -> Vec<Option<Arc<TileRow>>> {
    rows.into_iter()
        .map(|row| match row {
            Row::Artist(artist) => {
                let name = artist
                    .name
                    .clone()
                    .unwrap_or_else(|| "Unknown artist".to_owned());
                let count = artist.album_count as usize;
                let sub = format!(
                    "{} {}",
                    thousands(count),
                    if count == 1 { "album" } else { "albums" }
                );
                let filed = name.trim_start();
                let filed = ["the ", "a ", "an "]
                    .iter()
                    .find_map(|article| {
                        filed
                            .get(..article.len())
                            .filter(|start| start.eq_ignore_ascii_case(article))
                            .map(|_| filed.get(article.len()..).unwrap_or(filed))
                    })
                    .unwrap_or(filed);
                let initial = filed
                    .chars()
                    .find(|c| c.is_alphanumeric())
                    .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect());
                let soft_name = format!("{name}{sub}");
                Some(Arc::new(TileRow {
                    subject: Subject::Artist(artist.artist_id),
                    id: artist.artist_id,
                    title: name.clone().into(),
                    artist: sub.into(),
                    names: (artist.name, artist.sort_name),
                    initial: initial.into(),
                    round: true,
                    art: artist.art_id,
                    soft: theme::soft(&soft_name),
                    no_cover: artist.art_id.is_none().then_some(soft_name),
                    artist_id: Some(artist.artist_id),
                }))
            }
            Row::Album(tile) => Some(Arc::new(TileRow::of_album(tile))),
            _ => None,
        })
        .collect()
}

pub(crate) const TITLE_BAR: f32 = 52.;
pub(crate) const PLAYER_BAR: f32 = 76.;

/// The page area: its width, and its height between the bars.
pub(crate) fn viewport_of(window: &Window) -> (f32, f32) {
    let size = window.viewport_size();
    (
        f32::from(size.width),
        (f32::from(size.height) - TITLE_BAR - PLAYER_BAR).max(100.),
    )
}

/// The margins of a wall: 40 points, or what centres a wall of 2,000.
pub(crate) fn gutter(width: f32) -> f32 {
    ((width - 2000.) / 2.).round().max(40.)
}

impl Render for Wall {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_wall(window, cx)
    }
}

impl Wall {
    fn render_wall(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let started = Instant::now();
        self.frame += 1;
        crate::waits::frame("wall");
        let (window_width, viewport) = viewport_of(window);
        // With Up next moving the page over, the wall is laid out in the
        // room left of the card.
        let rail = crate::upnext::rail(cx);
        let width = window_width - rail;
        self.viewport = viewport;
        let scale = window.scale_factor();
        let px_size = (TILE * scale).ceil() as u32;

        // Nothing to show yet, or nothing to show: the page says why. An
        // empty wall keeps its controls, so that the other can be chosen.
        if let Some(blank) = self.blank(cx) {
            let controls = self.view.is_some() && self.total == 0 && !self.failed;
            let (top, header) = if controls {
                (
                    HEADER,
                    self.header(gutter(width), gutter(width) + rail, 20., cx),
                )
            } else {
                let name = name_row(gutter(width), gutter(width) + rail, 20., None);
                (HEADER - 66., name.into_any_element())
            };
            return div()
                .id("wall")
                .relative()
                .size_full()
                .overflow_hidden()
                .child(header)
                .child(
                    div()
                        .absolute()
                        .top(px(top))
                        .left_0()
                        .right_0()
                        .child(blank.render(viewport)),
                )
                .into_any_element();
        }

        // The measure's fling moves the wall as momentum would.
        let max = (self.content_height() - viewport).max(0.);
        if let Some(fling) = &mut self.fling
            && let Some((dy, phase)) = fling.step_at(started, self.top, max)
        {
            if let Some(last) = self.stats.last {
                let dt = started.duration_since(last).as_secs_f32();
                if dy != 0. && dt > 0. {
                    self.stats.speeds.push(dy.abs() / dt);
                }
            }
            self.scroll_by(crate::elastic::Wheel::plain(dy), phase, viewport, started);
        }

        let gutter = gutter(width);
        self.gutter = gutter;
        let wall_width = width - 2. * gutter;
        let relayout = match &self.layout {
            None => true,
            Some(layout) => layout.width != wall_width || layout.shape != self.shape(),
        };
        if relayout && self.total > 0 {
            let next = layout::layout(wall_width, self.total, &self.sections, self.shape());
            // A resize keeps the row at the top where it was on screen.
            if let Some(before) = &self.layout
                && self.top > HEADER
            {
                let keep = before.index_at(self.top - HEADER);
                let offset = before.y_of(keep) - (self.top - HEADER);
                self.top = HEADER + next.y_of(keep) - offset;
            }
            self.layout = Some(next);
            // After a replacement, the anchor row back where it was on screen.
            if let (Some((index, screen)), Some(layout)) =
                (self.pending_anchor.take(), &self.layout)
            {
                self.top = (HEADER + layout.y_of(index) - screen).max(0.);
            }
        }
        // Up from an artist's page, centre their tile on screen.
        if let (Some(index), Some(layout)) = (self.reveal_at, &self.layout) {
            self.reveal_at = None;
            crate::debug(format_args!("wall: revealing row {index}"));
            let under = ((viewport - layout.pitch) / 2.).max(0.);
            self.top = HEADER + layout.y_of(index) - under;
            self.elastic.reset();
        }
        let max = (self.content_height() - viewport).max(0.);
        self.top = self.top.clamp(0., max);

        self.plan(viewport, px_size, started, cx);
        theme::flush_soft(window, cx);
        // Three seconds at rest, the covers held come down to about two screens.
        if !self.motion.moving(started, Duration::from_secs(3)) && self.fling.is_none() {
            let screen = (viewport / 219. + 1.) * (width / 168.);
            let keep = (screen * 2.) as usize * (px_size as usize).pow(2) * 4;
            let frame = self.frame;
            self.covers.trim(frame, keep, window, cx);
            theme::trim_soft(window, cx);
        } else if self.trim_wake.is_none() {
            // A frame once the rest has begun, to trim then.
            self.trim_wake = Some(cx.spawn(async move |wall, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(3100))
                    .await;
                let _ = wall.update(cx, |wall, cx| {
                    wall.trim_wake = None;
                    cx.notify();
                });
            }));
        }

        let mut children: Vec<AnyElement> = Vec::new();
        // Where the wall is drawn: past either end while it is stretched.
        if self.elastic.step(started) {
            window.request_animation_frame();
        }
        let top = self.top + self.elastic.shown();

        // The name and the row of controls, scrolling away with the wall.
        if top < HEADER {
            children.push(self.header(gutter, gutter + rail, 20. - top, cx));
        }

        let mut tiles_in_view = 0usize;
        let mut covers_shown = 0usize;
        let mut fading = false;
        let mut in_view = HashSet::new();
        let mut seen = HashSet::new();
        if let Some(layout) = self.layout.clone() {
            let wall_top = top - HEADER;
            let lines = layout.lines_between(wall_top - layout.pitch * 0.5, wall_top + viewport);
            let moving = self.motion.moving(started, Duration::from_millis(450));
            let ink = letter_ink(&self.motion, started);
            if ink.1 {
                window.request_animation_frame();
            }
            // What stands over each group: in the lettered shape the large
            // letter in its lane, held 30 points under the top while its
            // tiles pass; otherwise a heading over its tiles, a small
            // letter's or a stretch of time's, that scrolls away with them.
            let shown_from = layout
                .groups
                .partition_point(|group| group.top + group.height < wall_top);
            for group in layout
                .groups
                .iter()
                .skip(shown_from)
                .take_while(|group| group.top <= wall_top + viewport)
            {
                let group_top = HEADER + group.top - top;
                let label = group.label.clone();
                match layout.shape {
                    Shape::Lettered => {
                        let y = (group_top - 6.)
                            .max(30.)
                            .min(group_top + group.height - 46.);
                        let color = theme::mix(
                            theme::mix(theme::ink(), theme::bg(), 0.62),
                            theme::ink(),
                            ink.0,
                        );
                        let at = gpui::point(px(gutter), px(y + 46. + 6.));
                        children.push(
                            div()
                                .id(SharedString::from(format!("letter-{}", group.start)))
                                .absolute()
                                .left(px(gutter))
                                .top(px(y))
                                .cursor_pointer()
                                .on_click(cx.listener(move |wall, _, _, cx| {
                                    wall.index_at = Some(at);
                                    cx.notify();
                                }))
                                .child(crate::tracked::tracked(
                                    label,
                                    46.,
                                    46.,
                                    FontWeight::LIGHT,
                                    color,
                                    -0.02,
                                ))
                                .into_any_element(),
                        );
                    }
                    Shape::Headed => {
                        let at = gpui::point(px(gutter), px(group_top + 6. + 22. + 6.));
                        children.push(
                            div()
                                .id(SharedString::from(format!("letter-{}", group.start)))
                                .absolute()
                                .left(px(gutter))
                                .top(px(group_top + 6.))
                                .h(px(22.))
                                .text_size(px(17.))
                                .line_height(px(22.))
                                .font_weight(FontWeight(650.))
                                .text_color(theme::dim())
                                .hover(|style| style.text_color(theme::ink()))
                                .cursor_pointer()
                                .on_click(cx.listener(move |wall, _, _, cx| {
                                    wall.index_at = Some(at);
                                    cx.notify();
                                }))
                                .child(crate::tracked::tracked(
                                    label,
                                    17.,
                                    22.,
                                    FontWeight(650.),
                                    theme::dim(),
                                    -0.01,
                                ))
                                .into_any_element(),
                        );
                    }
                    Shape::Dated => {
                        children.push(
                            div()
                                .absolute()
                                .left(px(gutter))
                                .top(px(group_top + 12.))
                                .child(crate::tracked::tracked(
                                    label,
                                    22.,
                                    30.,
                                    FontWeight::BOLD,
                                    theme::ink(),
                                    -0.015,
                                ))
                                .into_any_element(),
                        );
                    }
                }
            }
            let _ = moving;
            for line in &lines {
                let y = HEADER + line.y - top;
                let visible = y + layout.row_height > 0. && y < viewport;
                for index in line.first..line.first + line.count {
                    let (x, _) = layout.tile_origin(line, index);
                    let row = self.row(index);
                    if visible {
                        tiles_in_view += 1;
                        seen.insert(index);
                    }
                    let entered = visible && !self.seen.contains(&index);
                    let mut shown = false;
                    let element = match &row {
                        None => placeholder(index).into_any_element(),
                        Some(row) => {
                            let cover = row.art.and_then(|art| {
                                if visible {
                                    in_view.insert(art);
                                }
                                self.covers.get(art, px_size, self.frame).map(|ready| {
                                    let opacity = if ready.fades {
                                        (started.duration_since(ready.arrived).as_secs_f32()
                                            / covers::FADE.as_secs_f32())
                                        .min(1.)
                                    } else {
                                        1.
                                    };
                                    (ready.image.clone(), opacity)
                                })
                            });
                            if visible && let Some(art) = row.art {
                                crate::waits::drawn("wall", art, px_size, cover.is_some());
                            }
                            if let Some((_, opacity)) = &cover {
                                if *opacity < 1. {
                                    fading = true;
                                } else {
                                    shown = true;
                                }
                            }
                            let ground = row
                                .art
                                .and_then(|art| self.covers.colors.get(&art).copied())
                                .map_or(row.soft, theme::ground);
                            let selected = self.menu_open == Some(index)
                                || self
                                    .selected
                                    .as_ref()
                                    .is_some_and(|selected| selected.id == row.id);
                            let away = self.away == Some(index);
                            // The ring of the tile you came back from: held,
                            // then gone by 2.2 seconds.
                            let here =
                                self.here
                                    .filter(|(id, _)| *id == row.id)
                                    .map_or(0., |(_, at)| {
                                        let t = at.elapsed().as_secs_f32() / 2.2;
                                        if t <= 0.55 {
                                            1.
                                        } else {
                                            let fade = ((t - 0.55) / 0.45).min(1.);
                                            // `ease-out`.
                                            1. - crate::shell::bezier(0., 0., 0.58, 1., fade)
                                        }
                                    });
                            let grow = self.settling.filter(|(id, _)| *id == row.id).map_or(
                                1.,
                                |(_, at)| {
                                    let t = (at.elapsed().as_secs_f32() / 0.6).min(1.);
                                    1.06 - 0.06 * crate::shell::bezier(0.2, 0.7, 0.2, 1., t)
                                },
                            );
                            if here > 0. || grow > 1. {
                                window.request_animation_frame();
                            }
                            tile(
                                index, row, ground, cover, selected, here, grow, away, px_size, cx,
                            )
                            .into_any_element()
                        }
                    };
                    if visible && shown {
                        covers_shown += 1;
                    }
                    if entered {
                        self.stats.entered += 1;
                        if !shown {
                            self.stats.entered_unfinished += 1;
                        }
                    }
                    children.push(
                        div()
                            .absolute()
                            .left(px(gutter + x))
                            .top(px(y))
                            .w(px(TILE))
                            .child(element)
                            .into_any_element(),
                    );
                }
            }
            // A thin bar while the wall moves, as the system's overlay scroller has it.
            if moving || self.fling.is_some() {
                let total = self.content_height();
                let track = viewport - 8.;
                let length = (viewport / total * track).max(36.);
                let at = 4. + top / (total - viewport).max(1.) * (track - length);
                children.push(
                    div()
                        .absolute()
                        .right(px(3.))
                        .top(px(at))
                        .w(px(6.))
                        .h(px(length))
                        .rounded(px(3.))
                        .bg(theme::fill2())
                        .into_any_element(),
                );
            }
        }
        if let Some(lozenge) = self.lozenge(viewport, rail) {
            children.push(lozenge);
        }
        if let Some(at) = self.index_at {
            children.extend(self.index(at, cx));
        }
        if fading {
            window.request_animation_frame();
        }
        self.in_view = in_view;
        self.seen = seen;

        // The measure: frame times, and what was on screen.
        let painted = Rc::new(Cell::new(None));
        if self.fling.is_some() {
            self.stats.frame(started, tiles_in_view, covers_shown);
            window.request_animation_frame();
            let mark = Rc::clone(&painted);
            children.push(
                canvas(
                    |_, _, _| {},
                    move |_, _, _, _| mark.set(Some(Instant::now())),
                )
                .size_0()
                .into_any_element(),
            );
            self.stats.pending_paint = Some((started, painted));
        }
        if self.fling.as_ref().is_some_and(Fling::done) {
            self.finish(window, cx);
        }

        div()
            .id("wall")
            .relative()
            .size_full()
            .overflow_hidden()
            .on_scroll(cx.listener(Self::on_scroll))
            // A press anywhere on the page clears the selection unless the
            // selected tile stops propagation.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|wall, _, _, cx| {
                    if wall.selected.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .children(children)
            .into_any_element()
    }
}

impl Wall {
    fn finish(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(fling) = self.fling.take() {
            self.stats.flicks = fling.speeds();
        }
        let loader = self.loader.lock().map(|counts| *counts).unwrap_or_default();
        if let Some(fling) = &self.fling {
            let _ = fling;
        }
        self.stats.report(&self.covers, loader);
        if !self.stats.stay {
            cx.quit();
        }
    }

    /// Sets the measure's fling going.
    pub(crate) fn start_fling(&mut self, fling: Fling) {
        let (out, label, stay) = (
            self.stats.out.take(),
            std::mem::take(&mut self.stats.label),
            self.stats.stay,
        );
        self.stats = Stats {
            out,
            label,
            stay,
            ..Stats::default()
        };
        self.fling = Some(fling);
    }
}

/// Whether a read failed because the satellite's hub did not answer.
fn hub_unreachable<T>(
    answer: &Result<Result<T, ritornel_client::Error>, tokio::task::JoinError>,
) -> bool {
    matches!(
        answer,
        Ok(Err(ritornel_client::Error::Call(error)))
            if error.data.details.get("reason").and_then(|reason| reason.as_str())
                == Some("hub_unreachable")
    )
}

/// Whether a call on a view failed because the server had let the view go.
fn expired<T>(answer: &Result<Result<T, ritornel_client::Error>, tokio::task::JoinError>) -> bool {
    matches!(
        answer,
        Ok(Err(ritornel_client::Error::Call(error)))
            if error.data.code == ritornel_contract::ErrorCode::ViewExpired
    )
}

/// The letter's ink: faint at rest, full while the wall moves, fading back
/// over 0.7 s a moment after it stops. The second is whether it is changing.
fn letter_ink(motion: &Motion, now: Instant) -> (f32, bool) {
    let Some(at) = motion.moved_at else {
        return (0., false);
    };
    let since = now.duration_since(at).as_secs_f32();
    if since < 0.45 {
        (1., true)
    } else if since < 1.15 {
        (1. - (since - 0.45) / 0.7, true)
    } else {
        (0., false)
    }
}

/// The page's name, and how many there are when that is known.
fn name_row(gutter: f32, right: f32, y: f32, count: Option<String>) -> impl IntoElement {
    div()
        .absolute()
        .left(px(gutter))
        .right(px(right))
        .top(px(y))
        .flex()
        .items_baseline()
        .gap(px(14.))
        .h(px(42.))
        .child(crate::tracked::tracked(
            "Library",
            30.,
            42.,
            FontWeight::BOLD,
            theme::ink(),
            -0.022,
        ))
        .children(count.map(|count| {
            // The tracked name has no text baseline for the row to align
            // the count by, so the count is set on the name's by hand.
            div()
                .relative()
                .top(px(-4.))
                .text_size(px(14.))
                .text_color(theme::dim())
                .font_features(gpui::FontFeatures(Arc::new(vec![("tnum".to_owned(), 1)])))
                .child(count)
        }))
}

impl Wall {
    /// What the page says instead of the wall when disconnected or empty.
    fn blank(&self, cx: &mut Context<Self>) -> Option<Blank> {
        let Some(net) = &self.net else {
            let link = self.link.read(cx);
            // A server getting the library ready says so, in place of a
            // page that could pass for an empty library.
            if let Some(step) = link.preparing {
                let (title, text) = crate::link::preparing_words(step);
                return Some(Blank::new(title, text));
            }
            // Looking for the first time: the page has only its name.
            let State::Away {
                why,
                message,
                data_dir,
            } = &link.state
            else {
                return Some(Blank::new("", ""));
            };
            let (title, text, look) = why.words();
            let mut blank = Blank::new(title, text);
            blank.facts = message
                .clone()
                .or_else(|| data_dir.as_ref().map(|dir| format!("Looked in {dir}")))
                .map(Into::into);
            if look {
                let link = self.link.clone();
                let label = if self.link.read(cx).looking {
                    "Looking…"
                } else {
                    "Look again"
                };
                blank.button = Some((
                    label.into(),
                    Box::new(move |_, _, cx| link.update(cx, |link, cx| link.look_again(cx))),
                ));
            }
            return Some(blank);
        };
        if !net.holds {
            let (title, is, then) = match net.role {
                ServerRole::Unassigned => (
                    "No library yet.",
                    "which holds no library.",
                    "Add the folder your music is in under Settings › Library, and your artists and albums will be here.",
                ),
                ServerRole::Hub => (
                    "Your library is on this server.",
                    "which holds your library.",
                    "Its artists and albums could not be read.",
                ),
                ServerRole::Satellite => (
                    "Your library is on another server.",
                    "which is linked to the server that holds your library.",
                    "Showing its artists and albums here is not built yet.",
                ),
            };
            let mut blank = Blank::new(title, format!("Connected to {}, {is} {then}", net.name));
            blank.bold = Some(net.name.clone().into());
            if net.role == ServerRole::Unassigned {
                blank.button = Some((
                    "Open Settings".into(),
                    Box::new(|_, _, cx| {
                        crate::settings::open(Some(crate::settings::Section::Library), cx);
                    }),
                ));
            }
            blank.facts = net.versions.as_ref().map(|versions| {
                format!(
                    "Server {} · Contract {} · SQLite {} · Schema {}",
                    versions.server, versions.contract, versions.sqlite, versions.server_schema
                )
                .into()
            });
            return Some(blank);
        }
        // A wall the hub's absence kept from being read says that, and
        // reads itself again when the hub is back: there is nothing to do.
        if self.failed
            && let Some(away) = crate::away::away(net)
        {
            let (title, text) = away.blank();
            return Some(Blank::new(title, text));
        }
        if self.failed {
            let mut blank = Blank::new(
                "The library could not be read.",
                "The server is there, but it did not answer with the albums.",
            );
            let wall = cx.entity().downgrade();
            blank.button = Some((
                "Try again".into(),
                Box::new(move |_, _, cx| {
                    let _ = wall.update(cx, |wall, cx| wall.try_again(cx));
                }),
            ));
            return Some(blank);
        }
        if self.view.is_some() && self.total == 0 {
            let (title, then) = empty_words(self.songs);
            let mut blank = Blank::new(
                title,
                format!(
                    "Connected to {}, which holds your library. {then}",
                    net.name
                ),
            );
            blank.bold = Some(net.name.clone().into());
            return Some(blank);
        }
        None
    }
}

impl Wall {
    /// The page's name and how many there are, and one row of controls
    /// under it, which wall at the left and the order at the right, scrolling
    /// away with the wall.
    fn header(&self, gutter: f32, right: f32, y: f32, cx: &mut Context<Self>) -> AnyElement {
        let segment = |kind: WallKind, label: &'static str, on: bool, cx: &mut Context<Self>| {
            let base = div()
                .id(label)
                .flex()
                .items_center()
                .h(px(28.))
                .px(px(13.))
                .rounded(px(8.))
                .whitespace_nowrap()
                .on_click(cx.listener(move |wall, _, _, cx| wall.choose(kind, None, cx)));
            if on {
                base.bg(theme::raised())
                    .text_color(theme::ink())
                    .font_weight(FontWeight(600.))
                    .shadow(vec![theme::css_shadow(1., 3., gpui::rgba(0x00000024))])
                    .child(label)
            } else {
                base.text_color(theme::dim())
                    .font_weight(FontWeight(500.))
                    .hover(|style| style.text_color(theme::ink()))
                    .child(label)
            }
        };
        let count = (self.view.is_some() && self.total > 0).then(|| self.kind.count(self.total));
        let choices = orders(self.kind);
        let order = self.order;
        let empty = self.view.is_some() && self.total == 0;
        let sort = (choices.len() > 1 && !empty).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(px(13.))
                .text_color(theme::dim())
                .child("Sort by")
                .child(
                    // A native pop-up menu as wide as its longest order,
                    // "Recently added".
                    div()
                        .id("sort")
                        .relative()
                        .flex()
                        .items_center()
                        .w(px(141.))
                        .h(px(30.))
                        .pl(px(12.))
                        .rounded(px(8.))
                        .bg(theme::fill())
                        .hover(|style| style.bg(theme::fill2()))
                        .text_color(theme::ink())
                        .font_weight(FontWeight(600.))
                        .on_click(cx.listener(move |_, event: &gpui::ClickEvent, window, cx| {
                            let window = window.window_handle();
                            let at = event.position();
                            let checked = choices.iter().position(|each| *each == order);
                            let labels = choices.iter().map(|each| each.label.to_owned()).collect();
                            cx.spawn(async move |wall, cx| {
                                let chosen =
                                    crate::menu::choose(window, labels, checked, at, cx).await;
                                if let Some(order) =
                                    chosen.and_then(|index| choices.get(index)).copied()
                                {
                                    let _ = wall.update(cx, |wall, cx| {
                                        wall.choose(WallKind::Albums, Some(order), cx)
                                    });
                                }
                            })
                            .detach();
                        }))
                        .child(order.label)
                        .child(
                            div()
                                .absolute()
                                .right(px(9.))
                                .top(px(8.))
                                .child(crate::icons::icon("chevron", 14., gpui::rgb(0x999999))),
                        ),
                )
        });
        div()
            .absolute()
            .left(px(gutter))
            .right(px(right))
            .top(px(y))
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .h(px(42.))
                    .mb(px(6.))
                    .child(name_row(0., 0., 0., count)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(22.))
                    .h(px(52.))
                    .child(
                        div()
                            .flex()
                            .p(px(2.))
                            .rounded(px(10.))
                            .bg(theme::fill())
                            .text_size(px(13.))
                            .child(segment(
                                WallKind::Artists,
                                "Artists",
                                self.kind == WallKind::Artists,
                                cx,
                            ))
                            .child(segment(
                                WallKind::Albums,
                                "Albums",
                                self.kind == WallKind::Albums,
                                cx,
                            )),
                    )
                    .child(div().flex_1())
                    .children(sort),
            )
            .into_any_element()
    }
}

pub(crate) fn thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A tile whose row has not been read: the same size, quiet bars for its words.
fn placeholder(index: usize) -> impl IntoElement {
    let first = 52. + ((index * 37) % 34) as f32;
    let second = 34. + ((index * 53) % 28) as f32;
    div()
        .flex()
        .flex_col()
        .child(div().size(px(TILE)).rounded(px(10.)).bg(theme::fill()))
        .child(
            div().mt(px(9.)).h(px(20.)).flex().items_center().child(
                div()
                    .w(px(TILE * first / 100.))
                    .h(px(10.))
                    .rounded(px(5.))
                    .bg(theme::fill()),
            ),
        )
        .child(
            div().h(px(18.)).flex().items_center().child(
                div()
                    .w(px(TILE * second / 100.))
                    .h(px(9.))
                    .rounded(px(5.))
                    .bg(theme::fill()),
            ),
        )
}

#[allow(clippy::too_many_arguments)]
fn tile(
    index: usize,
    row: &Arc<TileRow>,
    ground: (Hsla, Hsla),
    cover: Option<(Arc<gpui::RenderImage>, f32)>,
    selected: bool,
    here: f32,
    grow: f32,
    away: bool,
    px_size: u32,
    cx: &mut Context<Wall>,
) -> impl IntoElement {
    // An artist is drawn round, a record with corners of 10.
    // Settling, the art is drawn larger about its centre, its place the same.
    let side = TILE * grow;
    let spill = (side - TILE) / 2.;
    let radius = if row.round { side / 2. } else { 10. };
    let group: SharedString = format!("tile-{index}").into();
    let mut art = div()
        .relative()
        .size(px(side))
        .m(px(-spill))
        .rounded(px(radius))
        // Under the pointer the art rises 2 points.
        .group_hover(group.clone(), |style| style.top(px(-2.)));
    if !away {
        art = art
            .bg(linear_gradient(
                180.,
                linear_color_stop(ground.0, 0.),
                linear_color_stop(ground.1, 1.),
            ))
            .children(crate::frost::piece(crate::frost::ground(ground), 1.));
    }
    let cover = cover.or_else(|| {
        row.no_cover
            .as_ref()
            .map(|name| (theme::soft_cached(name, px_size), 1.))
    });
    if let Some((image, opacity)) = cover.filter(|_| !away) {
        art = art.children(crate::frost::piece(
            crate::frost::Fill::Picture(Arc::clone(&image)),
            opacity,
        ));
        art = art.child(
            img(image)
                .absolute()
                .top_0()
                .left_0()
                .size(px(side))
                .rounded(px(radius))
                .opacity(opacity),
        );
    }
    // An artist with no picture: the first letter of their name, large and
    // white over their soft colour.
    if row.round && row.art.is_none() && !away {
        art = art.child(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(side * 0.44))
                .line_height(px(side * 0.44))
                .font_weight(FontWeight::BOLD)
                .text_color(gpui::rgb(0xffffff))
                .child(row.initial.clone()),
        );
    }
    // A hairline inside the edge, so a dark cover meets a dark page cleanly.
    art = art.when(!away, |art| {
        art.child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size(px(side))
                .rounded(px(radius))
                .border_1()
                .border_color(gpui::rgba(0x80808024)),
        )
    });
    // The rings are the cover's: while it travels, they are not left behind.
    if selected && !away {
        art = art.child(
            div()
                .absolute()
                .top(px(-3.5))
                .left(px(-3.5))
                .size(px(side + 7.))
                .rounded(px(radius + 3.5))
                .border(px(1.5))
                .border_color(theme::mix(theme::acc(), theme::bg(), 0.45)),
        );
    }
    // Coming up from an artist's page, the one you came from is marked:
    // a ring of the accent, 3 points out and 2 wide.
    if here > 0. && !away {
        art = art.child(
            div()
                .absolute()
                .top(px(-5.))
                .left(px(-5.))
                .size(px(side + 10.))
                .rounded(px(radius + 5.))
                .border(px(2.))
                .border_color(gpui::Rgba {
                    a: here,
                    ..theme::acc()
                }),
        );
    }
    let open_row = Arc::clone(row);
    let menu_row = Arc::clone(row);
    div()
        .id(("tile", index))
        .group(group)
        .flex()
        .flex_col()
        .when(row.round, |tile| tile.items_center().text_center())
        .cursor_pointer()
        .child(art)
        .child(
            div()
                .mt(px(9.))
                .h(px(20.))
                .text_size(px(14.))
                .line_height(px(20.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::ink())
                .w_full()
                .truncate()
                .child(row.title.clone()),
        )
        .child(
            div()
                .h(px(18.))
                .text_size(px(13.))
                .line_height(px(18.))
                .text_color(theme::dim())
                .w_full()
                .truncate()
                .child(row.artist.clone()),
        )
        .on_click(
            cx.listener(move |wall, event: &gpui::ClickEvent, _window, cx| {
                if event.standard_click() {
                    let at = event.position();
                    let _ = at;
                    let art = wall.art_bounds(index);
                    if let Some(art) = art {
                        cx.emit(OpenAlbum {
                            row: Arc::clone(&open_row),
                            index,
                            art,
                        });
                    }
                }
            }),
        )
        .when(selected, |tile| {
            tile.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        })
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |wall, event: &gpui::MouseDownEvent, window, cx| {
                wall.menu_open = Some(index);
                cx.notify();
                crate::menu::record_menu(Arc::clone(&menu_row), event.position, window, cx);
            }),
        )
}

impl Wall {
    /// Where row `index`'s art is in the window now.
    pub(crate) fn art_bounds(&self, index: usize) -> Option<Bounds<Pixels>> {
        let (x, y) = self.layout.as_ref()?.origin_of(index)?;
        Some(Bounds::new(
            point(
                px(self.gutter + x),
                px(self.origin_y + HEADER + y - self.top - self.elastic.shown()),
            ),
            size(px(TILE), px(TILE)),
        ))
    }
}

/// How many songs the library has, asked only when a wall opens empty:
/// whether the library is empty too.
async fn songs_if_empty(client: &ritornel_client::Client, total: u64) -> Option<u64> {
    if total > 0 {
        return None;
    }
    let stats = client.call::<LibraryStats>(&NoParams {}).await;
    stats.ok().map(|stats| stats.recordings)
}

/// What an empty wall says, by how many songs the library has: that the
/// library is empty, or that its songs name no album and so are on no wall.
fn empty_words(songs: Option<u64>) -> (&'static str, &'static str) {
    if songs.is_some_and(|songs| songs > 0) {
        (
            "No albums yet.",
            "Its songs name no album, so there are no artists or albums to show. Search finds them.",
        )
    } else {
        (
            "Your library is empty.",
            "There is no music in it yet: when a folder of music has been scanned, your artists and albums will be here.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn opened_views_close_when_sections_fail_or_the_wall_lets_go()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for fail_sections in [true, false] {
            let (send, mut calls) = mpsc::channel(4);
            let (answers, receive) = mpsc::channel(4);
            let client = Arc::new(ritornel_client::Client::over_memory(send, receive));
            let server = tokio::spawn(async move {
                for method in ["list.open", "list.sections", "list.close"] {
                    let call: Value =
                        serde_json::from_str(&calls.recv().await.ok_or("missing call")?)?;
                    assert_eq!(call.get("method").and_then(Value::as_str), Some(method));
                    let result = match method {
                        "list.open" => {
                            json!({"view":"first","total":0,"libraryRevision":1,"offset":0,"rows":[]})
                        }
                        "list.sections" if fail_sections => Value::Null, // A malformed reply.
                        "list.sections" => json!({"sections":[]}),
                        _ => {
                            assert_eq!(
                                call.pointer("/params/view").and_then(Value::as_str),
                                Some("first")
                            );
                            json!({"closed":true})
                        }
                    };
                    answers
                        .send(
                            json!({"jsonrpc":"2.0","id":call.get("id"),"result":result})
                                .to_string(),
                        )
                        .await?;
                }
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
            let opened = HeldView::open(
                &client,
                &ListOpenParams {
                    kind: ListKind::Artists,
                    query: Query::default(),
                    window: None,
                    replaces: None,
                },
            )
            .await;
            assert_eq!(opened.is_err(), fail_sections);
            drop(opened);
            tokio::time::timeout(Duration::from_secs(5), server).await???;
        }
        Ok(())
    }

    #[test]
    fn a_wall_says_the_library_is_empty_only_when_it_has_no_songs() {
        assert_eq!(empty_words(Some(0)).0, "Your library is empty.");
        assert_eq!(empty_words(None).0, "Your library is empty.");
        assert_eq!(empty_words(Some(12)).0, "No albums yet.");
    }
}
