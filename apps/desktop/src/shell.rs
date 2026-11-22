//! The window: the frame, the wall or a page in it, the wash borrowed from
//! the page's cover behind everything, and the cover that travels between
//! pages. Moving between pages is staged as the real client's
//! `stage.svelte.ts` stages it (`moves.rs`): the page left fades out under
//! the one that arrives, which waits to be ready before it hops into place.
//! Lean back lies over all of it while it is open (`lean.rs`).
//!
//! Up next is a card at the right (`upnext.rs`). With room it moves the
//! page over, and stays as you go about; in a window narrower than 1180
//! points it lies over the page and closes when you go somewhere
//! (`App.svelte`).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, AppContext, Bounds, Context, Entity, FocusHandle, Hsla, InteractiveElement,
    IntoElement, ObjectFit, ParentElement, Pixels, Render, RenderImage, SharedString, Styled,
    StyledImage, Subscription, Window, div, img, linear_color_stop, linear_gradient, px,
};

use crate::album::{Album, AlbumEvent};
use crate::artist::{Artist, ArtistEvent};
use crate::bar::PlayerBar;
use crate::frame::{self, Back};
use crate::frost;
use crate::history::HistoryPage;
use crate::link::Link;
use crate::moves::{self, Arrival, Leaving, Phase};
use crate::player::Player;
use crate::record::{Landings, Lift};
use crate::search::{SearchBox, SearchEvent};
use crate::stage::{History, Place, Tab};
use crate::theme;
use crate::upnext::{self, UpNextCard};
use crate::wall::{OpenAlbum, PLAYER_BAR, TITLE_BAR, TileRow, Wall};
use ritornel_contract::{Id, SkipDirection};

/// Where a cover is going, known once the page it goes to is ready.
enum Target {
    /// The large cover of an album page.
    Album(Entity<Album>),
    /// A tile of the wall, by its row.
    Wall(usize),
    /// A record on a page, as its landings know it.
    Landing(Rc<Landings>, Id),
    /// An artist's portrait at the right end of their banner.
    Portrait(Entity<Artist>),
    /// Nowhere: it fades where it stands.
    Nowhere,
}

/// What a flying cover hides while it flies, shown again when it is done.
#[derive(Clone)]
enum Hidden {
    Wall,
    Landing(Rc<Landings>),
    Album(Entity<Album>),
    Portrait(Entity<Artist>),
}

/// A cover travelling between pages (`motion.ts`, `Flight`).
struct Carry {
    image: Option<Arc<RenderImage>>,
    ground: (Hsla, Hsla),
    from: Bounds<Pixels>,
    /// Its corners where it starts and where it lands.
    radius: (f32, f32),
    /// An artist's picture, which is round on the wall and their portrait
    /// on their page: whether each end is round, and the picture at the end
    /// it flies to, which comes in over the one it leaves on the way.
    round: (bool, bool),
    to_image: Option<Arc<RenderImage>>,
    target: Target,
    phase: Phase,
    back: bool,
    hidden: Vec<Hidden>,
    /// When it started, and each frame it was drawn, for the trace.
    started: Instant,
    frames: Vec<Instant>,
}

/// Where an album's cover came from, for it to travel back to on Back.
#[derive(Clone)]
pub(crate) enum Came {
    /// A tile of the wall, by its row.
    Wall(usize),
    /// A record on another page: its landings know where.
    Page(Rc<Landings>),
}

/// A cover's take-off: where it came from, where it was, and its colour.
pub(crate) struct Takeoff {
    pub came: Came,
    pub from: Bounds<Pixels>,
    pub ground: (Hsla, Hsla),
}

/// A page of the window, kept whole in the way back.
pub(crate) enum Page {
    /// The Library: its wall is one entity for the app's life.
    Library,
    /// An album's page, and where its cover came from, to travel back to.
    Album {
        album: Entity<Album>,
        from: Option<Came>,
        _events: Subscription,
    },
    /// An artist's page, and whether it was opened from their tile on the
    /// wall, for their picture to travel back to.
    Artist {
        artist: Entity<Artist>,
        from_wall: bool,
        _events: Subscription,
    },
    /// History: the picture of your listening life.
    History(Entity<HistoryPage>),
    /// A tab whose page is not built yet.
    NotBuilt(Tab),
}

impl Place for Page {
    fn tab(&self) -> Tab {
        match self {
            Self::Library | Self::Album { .. } | Self::Artist { .. } => Tab::Library,
            Self::History(_) => Tab::History,
            Self::NotBuilt(tab) => *tab,
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Library, Self::Library) => true,
            (Self::Album { album, .. }, Self::Album { album: other, .. }) => album == other,
            (Self::Artist { artist, .. }, Self::Artist { artist: other, .. }) => artist == other,
            (Self::History(_), Self::History(_)) => true,
            (Self::NotBuilt(tab), Self::NotBuilt(other)) => tab == other,
            _ => false,
        }
    }
}

/// What is drawn for a page: it outlives the page while that leaves.
#[derive(Clone)]
enum Shown {
    Wall,
    Album(Entity<Album>),
    Artist(Entity<Artist>),
    History(Entity<HistoryPage>),
    NotBuilt(Tab),
}

impl Shown {
    fn of(page: &Page) -> Self {
        match page {
            Page::Library => Self::Wall,
            Page::Album { album, .. } => Self::Album(album.clone()),
            Page::Artist { artist, .. } => Self::Artist(artist.clone()),
            Page::History(history) => Self::History(history.clone()),
            Page::NotBuilt(tab) => Self::NotBuilt(*tab),
        }
    }

    /// An artist's page and an album's have a banner.
    fn bannered(&self) -> bool {
        matches!(self, Self::Album(_) | Self::Artist(_))
    }

    /// How far it is scrolled.
    fn top(&self, cx: &gpui::App) -> f32 {
        match self {
            Self::Album(album) => album.read(cx).top,
            Self::Artist(artist) => artist.read(cx).top,
            Self::History(history) => history.read(cx).top,
            _ => 0.,
        }
    }

    /// Whether it is ready to be seen.
    fn ready(&self, cx: &gpui::App) -> bool {
        match self {
            Self::Album(album) => album.read(cx).ready(),
            Self::Artist(artist) => artist.read(cx).ready(),
            Self::History(history) => history.read(cx).ready(),
            Self::Wall | Self::NotBuilt(_) => true,
        }
    }

    /// The colour it borrows, when its picture is there: its key and its picture.
    fn wash(&self, cx: &gpui::App) -> Option<(String, Arc<RenderImage>)> {
        match self {
            Self::Album(album) => {
                let album = album.read(cx);
                Some((album.wash_key(), Arc::clone(&album.blur.as_ref()?.0)))
            }
            Self::Artist(artist) => {
                let artist = artist.read(cx);
                Some((artist.wash_key(), Arc::clone(&artist.blur.as_ref()?.0)))
            }
            _ => None,
        }
    }

    /// Tells the page whether it is the one on screen or behind another,
    /// kept for Back.
    fn seen(&self, seen: bool, cx: &mut gpui::App) {
        match self {
            Self::Album(album) => album.update(cx, |album, cx| album.seen(seen, cx)),
            Self::Artist(artist) => artist.update(cx, |artist, cx| artist.seen(seen, cx)),
            Self::History(history) => history.update(cx, |history, cx| history.seen(seen, cx)),
            _ => {}
        }
    }

    /// Sets how its parts show, for a page with a banner.
    fn look(&self, look: moves::Look, cx: &mut gpui::App) {
        match self {
            Self::Album(album) => album.update(cx, |album, _| album.look = look),
            Self::Artist(artist) => artist.update(cx, |artist, _| artist.look = look),
            _ => {}
        }
    }

    fn element(&self, wall: &Entity<Wall>, height: f32) -> AnyElement {
        match self {
            Self::Wall => wall.clone().into_any_element(),
            Self::Album(album) => album.clone().into_any_element(),
            Self::Artist(artist) => artist.clone().into_any_element(),
            Self::History(history) => history.clone().into_any_element(),
            Self::NotBuilt(tab) => crate::blank::not_built(*tab, height - TITLE_BAR - PLAYER_BAR),
        }
    }
}

/// Up next's card while it is shown: when it began to fly in, and when it
/// began to fly out once it is closed.
struct Card {
    card: Entity<UpNextCard>,
    opened: Instant,
    closing: Option<Instant>,
}

/// One colour borrowed, as it came up (`frame/Wash.svelte`).
struct Tint {
    key: String,
    image: Arc<RenderImage>,
    since: Instant,
}

pub(crate) struct Shell {
    link: Entity<Link>,
    pub wall: Entity<Wall>,
    pub player: Entity<Player>,
    bar: Entity<PlayerBar>,
    search: Entity<SearchBox>,
    pages: History<Page>,
    /// Whether the pointer is on the title bar's connection, which then
    /// says how the library is reached.
    connection_hovered: bool,
    /// How the page now open arrives.
    arrival: Arrival,
    /// The page left, fading out under it.
    leaving: Option<(Shown, Leaving)>,
    /// The colours borrowed, the last on top; and when they began to ebb.
    tints: Vec<Tint>,
    ebbing: Option<Instant>,
    carry: Option<Carry>,
    card: Option<Card>,
    /// When each frame of the change of page now under way was drawn, for
    /// the trace.
    move_frames: Vec<Instant>,
    /// Lean back, while it is open, and what it says.
    lean: Option<(Entity<crate::lean::LeanBack>, Subscription)>,
    /// The window's width as last drawn.
    width: f32,
    focus: FocusHandle,
    _open: Subscription,
    _to_artist: Subscription,
    _from_bar: Subscription,
    _search: Subscription,
    _link: Subscription,
    _player: Subscription,
}

impl Shell {
    pub(crate) fn new(
        link: Entity<Link>,
        wall: Entity<Wall>,
        player: Entity<Player>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let open = cx.subscribe_in(&wall, window, Self::on_open);
        let to_artist = cx.subscribe_in(
            &wall,
            window,
            |shell: &mut Self, _, event: &crate::wall::OpenArtist, window, cx| {
                shell.open_artist(event.artist, event.name.clone(), window, cx);
            },
        );
        let followed = cx.observe(&link, |_, _, cx| cx.notify());
        // The notice, and the song playing marked on the page.
        let heard = cx.observe(&player, |_, _, cx| cx.notify());
        let bar = cx.new(|cx| PlayerBar::new(link.clone(), player.clone(), cx));
        // The bar's cover, or its album's name, opens the album.
        let from_bar = cx.subscribe_in(
            &bar,
            window,
            |shell: &mut Self, _, event: &crate::bar::BarEvent, window, cx| match event {
                crate::bar::BarEvent::LeanBack => shell.toggle_lean(window, cx),
                crate::bar::BarEvent::OpenAlbum(row) => {
                    // Already open, it opens nothing.
                    let open = shell
                        .album_page()
                        .is_some_and(|album| album.read(cx).row.id == row.id);
                    if !open {
                        shell.open_album(Arc::clone(row), None, None, window, cx);
                    }
                }
            },
        );
        let search = cx.new(|cx| SearchBox::new(link.clone(), cx));
        let from_search = cx.subscribe_in(&search, window, Self::on_search);
        // The menus' items open pages through the shell.
        let here = cx.entity().downgrade();
        cx.set_global(crate::menu::Here(here));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut arrival = Arrival::new(false, false, true);
        arrival.ready = Some(arrival.started);
        Self {
            link,
            wall,
            player,
            bar,
            search,
            pages: History::new(Page::Library),
            connection_hovered: false,
            arrival,
            leaving: None,
            tints: Vec::new(),
            ebbing: None,
            carry: None,
            card: None,
            move_frames: Vec::new(),
            lean: None,
            width: 0.,
            focus,
            _open: open,
            _to_artist: to_artist,
            _from_bar: from_bar,
            _search: from_search,
            _link: followed,
            _player: heard,
        }
    }

    /// Opens Lean back while something is playing, or closes it.
    pub(crate) fn toggle_lean(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((lean, _)) = &self.lean {
            if !lean.read(cx).leaving() {
                lean.update(cx, |lean, cx| lean.close(cx));
            }
            return;
        }
        if !self.player.read(cx).on() {
            return;
        }
        // Nothing keeps the key focus that opened it: space plays and pauses.
        window.focus(&self.focus, cx);
        let lean =
            cx.new(|cx| crate::lean::LeanBack::new(self.link.clone(), self.player.clone(), cx));
        let heard = cx.subscribe(&lean, |shell: &mut Self, _, event, cx| match event {
            crate::lean::LeanEvent::BarCover(away) => {
                let away = *away;
                shell.bar.update(cx, |bar, cx| {
                    bar.cover_away = away;
                    cx.notify();
                });
            }
            crate::lean::LeanEvent::Gone => {
                shell.lean = None;
                shell.bar.update(cx, |bar, cx| {
                    bar.cover_away = false;
                    cx.notify();
                });
                cx.notify();
            }
        });
        self.lean = Some((lean, heard));
        cx.notify();
    }

    /// Starts Lean back on its way out, when it is open. Whether it was.
    fn close_lean(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((lean, _)) = &self.lean else {
            return false;
        };
        lean.update(cx, |lean, cx| lean.close(cx));
        true
    }

    /// The album page open, if one is, and where its cover came from.
    fn album(&self) -> Option<(&Entity<Album>, Option<&Came>)> {
        match self.pages.current() {
            Page::Album { album, from, .. } => Some((album, from.as_ref())),
            _ => None,
        }
    }

    /// The artist whose page is open, if one is.
    pub(crate) fn artist_here(&self, cx: &gpui::App) -> Option<Id> {
        match self.pages.current() {
            Page::Artist { artist, .. } => Some(artist.read(cx).artist_id),
            _ => None,
        }
    }

    /// The session with the server, while there is one.
    pub(crate) fn net(&self, cx: &gpui::App) -> Option<Arc<crate::net::Net>> {
        self.link.read(cx).net()
    }

    /// The album page open, if one is.
    pub(crate) fn album_page(&self) -> Option<Entity<Album>> {
        self.album().map(|(album, _)| album.clone())
    }

    /// The artist page open, if one is.
    pub(crate) fn artist_page(&self) -> Option<Entity<Artist>> {
        match self.pages.current() {
            Page::Artist { artist, .. } => Some(artist.clone()),
            _ => None,
        }
    }

    /// The History page open, if it is.
    pub(crate) fn history_page(&self) -> Option<Entity<HistoryPage>> {
        match self.pages.current() {
            Page::History(history) => Some(history.clone()),
            _ => None,
        }
    }

    /// Whether Up next's card is open and moves the page over: with room
    /// for it, at 1180 points wide or more.
    fn pushed(&self, cx: &gpui::App) -> bool {
        self.player.read(cx).open && self.width >= upnext::ROOM_FOR_CARD
    }

    /// Holds the pointer on the title bar's connection, or lets it go.
    pub(crate) fn hover_connection(&mut self, over: bool, cx: &mut Context<Self>) {
        self.connection_hovered = over;
        cx.notify();
    }

    /// Opens or closes Up next's card, as ⌥⌘U and the player bar's button do.
    pub(crate) fn toggle_card(&mut self, open: Option<bool>, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.toggle_card(open, cx));
    }

    /// The page open now, as drawn.
    fn shown(&self) -> Shown {
        Shown::of(self.pages.current())
    }

    /// A page has replaced `left`: it fades out under the one that arrives,
    /// which waits to be ready; between an artist and an album, at their
    /// tops, the banner holds still (`stage.svelte.ts`, `#move`).
    fn moved(&mut self, left: Shown, left_top: f32, cx: &mut Context<Self>) {
        self.move_frames = vec![Instant::now()];
        // Lying over the page, Up next's card closes when you go somewhere.
        if self.player.read(cx).open && !self.pushed(cx) {
            self.toggle_card(Some(false), cx);
        }
        let still = crate::platform::reduce_motion();
        let now = self.shown();
        left.seen(false, cx);
        now.seen(true, cx);
        let hold = left.bannered() && now.bannered() && left_top < 8. && now.top(cx) < 8. && !still;
        let entrance = matches!(now, Shown::Artist(_)) && !hold;
        if let Some((gone, _)) = self.leaving.take() {
            gone.look(moves::Look::default(), cx);
        }
        self.leaving = (!still).then(|| {
            (
                left,
                Leaving {
                    started: Instant::now(),
                    hold,
                },
            )
        });
        self.arrival = Arrival::new(hold, entrance, still);
        // Leaving a page that borrowed its colour for one that has none,
        // the colour ebbs away as the page leaves.
        if !now.bannered() && !self.tints.is_empty() && self.ebbing.is_none() {
            self.ebbing = Some(Instant::now());
        }
        self.traced("now on", cx);
        cx.notify();
    }

    /// Goes to a tab, as clicking it does. The lit tab's own page opens nothing.
    pub(crate) fn go(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let open = match (tab, self.pages.current()) {
            (Tab::Library, Page::Library) | (Tab::History, Page::History(_)) => true,
            (_, Page::NotBuilt(open)) => *open == tab,
            _ => false,
        };
        if open {
            return;
        }
        let page = match tab {
            Tab::Library => Page::Library,
            Tab::History => {
                let link = self.link.clone();
                Page::History(cx.new(|cx| HistoryPage::new(&link, cx)))
            }
            _ => Page::NotBuilt(tab),
        };
        self.finish_carry(cx);
        let left = self.shown();
        let left_top = left.top(cx);
        let carried = self.carry_cover_from(&left, cx);
        self.pages.go(page);
        self.aim_carried(carried, &left, cx);
        self.moved(left, left_top, cx);
    }

    fn on_open(
        &mut self,
        _wall: &Entity<Wall>,
        event: &OpenAlbum,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open(Arc::clone(&event.row), event.index, event.art, window, cx);
    }

    pub(crate) fn open(
        &mut self,
        row: Arc<TileRow>,
        index: usize,
        from: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.pages.current(), Page::Library) {
            return;
        }
        let tile_px = (crate::layout::TILE * window.scale_factor()).ceil() as u32;
        let small = row
            .art
            .and_then(|art| self.wall.read(cx).covers.image(art))
            .or_else(|| {
                row.no_cover
                    .as_ref()
                    .map(|name| theme::soft_cached(name, tile_px))
            });
        let ground = row
            .art
            .and_then(|art| self.wall.read(cx).covers.colors.get(&art).copied())
            .map_or(row.soft, theme::ground);
        let takeoff = Takeoff {
            came: Came::Wall(index),
            from,
            ground,
        };
        self.open_album(row, small, Some(takeoff), window, cx);
    }

    /// Opens a record's page from wherever it was asked for. With a
    /// take-off, from the wall or a record on another page, its cover
    /// travels to the page; without one, a cover on the page left travels
    /// to it, if it has a place there.
    pub(crate) fn open_album(
        &mut self,
        row: Arc<TileRow>,
        small: Option<Arc<RenderImage>>,
        flight: Option<Takeoff>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(net) = self.link.read(cx).net() else {
            return;
        };
        let album_ref = match row.subject {
            crate::wall::Subject::Album(album) => album,
            crate::wall::Subject::Artist(artist) => {
                // An artist with no picture, only their initial, has nothing
                // to carry: their tile stays as it is.
                let flight = flight.filter(|_| row.art.is_some());
                self.open_artist_from(artist, row.title.clone(), small, flight, window, cx);
                return;
            }
        };
        self.finish_carry(cx);
        let still = crate::platform::reduce_motion();
        let player = self.player.clone();
        let link = self.link.clone();
        let album = cx.new(|cx| {
            Album::new(
                net,
                &link,
                player,
                album_ref,
                Arc::clone(&row),
                small.clone(),
                window,
                cx,
            )
        });
        let events = cx.subscribe_in(&album, window, Self::on_album);
        let left = self.shown();
        let left_top = left.top(cx);
        let carried = if flight.is_none() {
            self.carry_cover_from(&left, cx)
        } else {
            None
        };
        let came = flight.map(|takeoff| {
            if !still {
                // The cover clicked is hidden where it was, and its place
                // on the page, until it lands.
                let mut hidden = vec![Hidden::Album(album.clone())];
                match &takeoff.came {
                    Came::Wall(index) => {
                        let index = *index;
                        self.wall.update(cx, |wall, _| wall.away = Some(index));
                        hidden.push(Hidden::Wall);
                    }
                    Came::Page(landings) => {
                        landings.away.set(Some(row.id));
                        hidden.push(Hidden::Landing(Rc::clone(landings)));
                    }
                }
                album.update(cx, |album, _| album.cover_away = true);
                self.carry = Some(Carry {
                    image: small.clone(),
                    ground: takeoff.ground,
                    from: takeoff.from,
                    radius: (10., 12.),
                    round: (false, false),
                    to_image: None,
                    target: Target::Album(album.clone()),
                    phase: Phase::Waiting,
                    back: false,
                    hidden,
                    started: Instant::now(),
                    frames: Vec::new(),
                });
            }
            takeoff.came
        });
        self.pages.go(Page::Album {
            album,
            from: came,
            _events: events,
        });
        self.aim_carried(carried, &left, cx);
        self.moved(left, left_top, cx);
    }

    fn on_album(
        &mut self,
        _album: &Entity<Album>,
        event: &AlbumEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AlbumEvent::Open(row, lift) => self.open_lifted(row, lift.as_ref(), window, cx),
            AlbumEvent::Artist(artist, name) => {
                self.up_to_artist(*artist, name.clone(), window, cx)
            }
        }
    }

    /// What a search chose: an artist's page, a record's with its cover
    /// lifted from the list, a song's album with the song marked, or a
    /// song on no record, played.
    fn on_search(
        &mut self,
        _search: &Entity<SearchBox>,
        event: &SearchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        match event {
            SearchEvent::OpenArtist(artist, name) => {
                self.open_artist(*artist, name.clone(), window, cx);
            }
            SearchEvent::OpenAlbum { row, lift, mark } => {
                let (small, takeoff) = match lift {
                    Some((from, image)) => (
                        image.clone(),
                        Some(Takeoff {
                            came: Came::Page(Rc::default()),
                            from: *from,
                            ground: row.soft,
                        }),
                    ),
                    None => (None, None),
                };
                self.open_album(Arc::clone(row), small, takeoff, window, cx);
                if let Some(song) = mark
                    && let Some(album) = self.album_page()
                {
                    album.update(cx, |album, cx| album.mark(*song, cx));
                }
            }
            SearchEvent::Play(song) => {
                let source = ritornel_contract::PlaySource::Tracks { ids: vec![*song] };
                let context = Some(ritornel_contract::PlayContext {
                    kind: ritornel_contract::ContextKind::Manual,
                    reference: None,
                });
                self.player
                    .update(cx, |player, cx| {
                        player.play(source, None, context, false, cx)
                    })
                    .detach();
            }
        }
    }

    /// Opens a record's page from a record on a page, its cover lifted
    /// from there when it was shown.
    fn open_lifted(
        &mut self,
        row: &Arc<TileRow>,
        lift: Option<&Lift>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let small = lift.and_then(|lift| lift.image.clone());
        let takeoff = lift.map(|lift| Takeoff {
            came: Came::Page(Rc::clone(&lift.landings)),
            from: lift.from,
            ground: row.soft,
        });
        self.open_album(Arc::clone(row), small, takeoff, window, cx);
    }

    /// Opens an artist's page.
    pub(crate) fn open_artist(
        &mut self,
        artist: Id,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_artist_from(artist, name, None, None, window, cx);
    }

    /// Opens an artist's page. With a take-off from their tile on the wall,
    /// their picture travels from its circle to their portrait in the
    /// banner, the portrait coming in over the circle on the way; with no
    /// portrait to land on, it fades where it stands.
    fn open_artist_from(
        &mut self,
        artist: Id,
        name: SharedString,
        small: Option<Arc<RenderImage>>,
        takeoff: Option<Takeoff>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(net) = self.link.read(cx).net() else {
            return;
        };
        self.finish_carry(cx);
        let player = self.player.clone();
        let link = self.link.clone();
        let page = cx.new(|cx| Artist::new(net, &link, player, artist, name, cx));
        let events = cx.subscribe_in(&page, window, Self::on_artist);
        let left = self.shown();
        let left_top = left.top(cx);
        let carried = self.carry_cover_from(&left, cx);
        let from_wall = matches!(
            &takeoff,
            Some(Takeoff {
                came: Came::Wall(_),
                ..
            })
        );
        let flies = match takeoff {
            Some(Takeoff {
                came: Came::Wall(index),
                from,
                ground,
            }) if !crate::platform::reduce_motion() => {
                self.wall.update(cx, |wall, _| wall.away = Some(index));
                page.update(cx, |page, _| page.portrait_away = true);
                self.carry = Some(Carry {
                    image: small,
                    ground,
                    from,
                    radius: (0., 0.),
                    round: (true, false),
                    to_image: None,
                    target: Target::Portrait(page.clone()),
                    phase: Phase::Waiting,
                    back: false,
                    hidden: vec![Hidden::Wall, Hidden::Portrait(page.clone())],
                    started: Instant::now(),
                    frames: Vec::new(),
                });
                true
            }
            _ => false,
        };
        self.pages.go(Page::Artist {
            artist: page,
            from_wall,
            _events: events,
        });
        if !flies {
            self.aim_carried(carried, &left, cx);
        }
        self.moved(left, left_top, cx);
    }

    /// Where an artist's portrait stands in their banner, the page scrolled
    /// `top` points, in a window `width` points across.
    fn portrait_bounds(width: f32, top: f32) -> Bounds<Pixels> {
        let wide = crate::banner::portrait_wide(width);
        Bounds::new(
            gpui::point(
                px(width - wide),
                px(TITLE_BAR - top.min(crate::banner::SLIDE)),
            ),
            gpui::size(px(wide), px(crate::banner::HEIGHT)),
        )
    }

    /// Back to the wall from an artist's page, their portrait travels to
    /// their circle on the wall, the circle coming in over it on the way:
    /// when the portrait is shown, the banner whole, and their tile read.
    fn carry_portrait_home(
        &mut self,
        left: &Shown,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Shown::Artist(page) = left else {
            return false;
        };
        let from_wall = matches!(
            self.pages.current(),
            Page::Artist {
                from_wall: true,
                ..
            }
        );
        if crate::platform::reduce_motion() || !from_wall {
            return false;
        }
        let (artist, top, portrait) = {
            let page = page.read(cx);
            (page.artist_id, page.top, page.portrait())
        };
        let Some(portrait) = portrait.filter(|_| top < 8.) else {
            return false;
        };
        // Only to a tile that can be seen where the wall was left.
        let bottom = f32::from(window.viewport_size().height) - PLAYER_BAR;
        let found = self.wall.update(cx, |wall, _| {
            let index = wall.index_of(artist)?;
            let tile = wall.art_bounds(index)?;
            if !moves::seen(tile, TITLE_BAR, bottom) {
                return None;
            }
            let row = wall.row(index)?;
            let image = row.art.and_then(|art| wall.covers.image(art));
            let ground = row
                .art
                .and_then(|art| wall.covers.colors.get(&art).copied())
                .map_or(row.soft, theme::ground);
            wall.away = Some(index);
            Some((index, image, ground))
        });
        let Some((index, image, ground)) = found else {
            return false;
        };
        page.update(cx, |page, _| page.portrait_away = true);
        self.carry = Some(Carry {
            image: Some(portrait),
            ground,
            from: Self::portrait_bounds(self.width, top),
            radius: (0., 0.),
            round: (false, true),
            to_image: image,
            target: Target::Wall(index),
            phase: Phase::Waiting,
            back: true,
            hidden: vec![Hidden::Wall, Hidden::Portrait(page.clone())],
            started: Instant::now(),
            frames: Vec::new(),
        });
        true
    }

    /// Up to an artist from one of their records: back, when their page is
    /// the one before; otherwise their page (`stage.up`).
    fn up_to_artist(
        &mut self,
        artist: Id,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let came_from = matches!(
            self.pages.previous(),
            Some(Page::Artist { artist: page, .. }) if page.read(cx).artist_id == artist
        );
        if came_from {
            self.back(window, cx);
        } else {
            self.open_artist(artist, name, window, cx);
        }
    }

    fn on_artist(
        &mut self,
        _artist: &Entity<Artist>,
        event: &ArtistEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ArtistEvent::Open(row, lift) => self.open_lifted(row, lift.as_ref(), window, cx),
            // Up to all the artists: back, when the wall is the page before,
            // the artist marked there; otherwise the Artists wall at them.
            ArtistEvent::Up(artist) => {
                if matches!(self.pages.previous(), Some(Page::Library)) {
                    self.back(window, cx);
                } else {
                    self.finish_carry(cx);
                    let left = self.shown();
                    let left_top = left.top(cx);
                    self.pages.go(Page::Library);
                    let artist = *artist;
                    self.wall.update(cx, |wall, cx| wall.reveal(artist, cx));
                    self.moved(left, left_top, cx);
                }
            }
        }
    }

    /// Leaving an album page by any way but a cover clicked, its cover
    /// travels whole to its place on the next page, or fades there if it has
    /// none (`#carryCover`). Not when it has scrolled up under the banner.
    fn carry_cover_from(&mut self, left: &Shown, cx: &mut Context<Self>) -> Option<Carry> {
        let Shown::Album(album) = left else {
            return None;
        };
        if crate::platform::reduce_motion() {
            return None;
        }
        let page = album.read(cx);
        let from = page.cover_bounds(self.width - upnext::rail(cx), TITLE_BAR);
        if f32::from(from.origin.y + from.size.height) <= TITLE_BAR + 60. {
            return None;
        }
        let carry = Carry {
            image: page.best_cover(),
            ground: page.row.soft,
            from,
            radius: (12., 10.),
            round: (false, false),
            to_image: None,
            target: Target::Nowhere,
            phase: Phase::Waiting,
            back: true,
            hidden: vec![Hidden::Album(album.clone())],
            started: Instant::now(),
            frames: Vec::new(),
        };
        album.update(cx, |album, _| album.cover_away = true);
        Some(carry)
    }

    /// Where a cover carried from the album page left goes on the page now
    /// open: its tile on the wall, or its record on an artist's page or the
    /// page it came from.
    fn aim_carried(&mut self, carried: Option<Carry>, left: &Shown, cx: &mut Context<Self>) {
        let Some(mut carry) = carried else { return };
        let Shown::Album(album) = left else { return };
        let record = album.read(cx).row.id;
        carry.target = match self.pages.current() {
            Page::Library => match self.wall.read(cx).index_of(record) {
                Some(index) => {
                    self.wall.update(cx, |wall, _| wall.away = Some(index));
                    carry.hidden.push(Hidden::Wall);
                    Target::Wall(index)
                }
                None => Target::Nowhere,
            },
            Page::Artist { artist, .. } => {
                let landings = Rc::clone(&artist.read(cx).landings);
                landings.away.set(Some(record));
                carry.hidden.push(Hidden::Landing(Rc::clone(&landings)));
                Target::Landing(landings, record)
            }
            Page::Album { album, .. } => {
                let landings = Rc::clone(&album.read(cx).landings);
                if landings.bounds(record).is_some() {
                    landings.away.set(Some(record));
                    carry.hidden.push(Hidden::Landing(Rc::clone(&landings)));
                    Target::Landing(landings, record)
                } else {
                    Target::Nowhere
                }
            }
            Page::History(_) | Page::NotBuilt(_) => Target::Nowhere,
        };
        self.carry = Some(carry);
    }

    /// Ends the cover's flight at once, and shows what it hid.
    fn finish_carry(&mut self, cx: &mut Context<Self>) {
        let Some(carry) = self.carry.take() else {
            return;
        };
        let gaps: Vec<f32> = carry
            .frames
            .windows(2)
            .map(|pair| match pair {
                [a, b] => b.duration_since(*a).as_secs_f32() * 1000.,
                _ => 0.,
            })
            .collect();
        let worst = gaps.iter().copied().fold(0f32, f32::max);
        let to = match carry.phase {
            Phase::Flying { to, .. } | Phase::Holding { to } => Some(to),
            _ => None,
        };
        crate::debug(format_args!(
            "fly {}: {} frames, longest gap {worst:.1} ms, {:.0} ms in all, from ({:.0}, {:.0}) {:.0} {}",
            if carry.back { "back" } else { "out" },
            carry.frames.len(),
            carry.started.elapsed().as_secs_f32() * 1000.,
            f32::from(carry.from.origin.x),
            f32::from(carry.from.origin.y),
            f32::from(carry.from.size.width),
            to.map_or_else(
                || "faded where it stood".to_owned(),
                |to| format!(
                    "to ({:.0}, {:.0}) {:.0}",
                    f32::from(to.origin.x),
                    f32::from(to.origin.y),
                    f32::from(to.size.width)
                ),
            ),
        ));
        for hidden in carry.hidden {
            match hidden {
                Hidden::Wall => self.wall.update(cx, |wall, cx| {
                    wall.away = None;
                    cx.notify();
                }),
                Hidden::Landing(landings) => landings.away.set(None),
                Hidden::Album(album) => album.update(cx, |album, cx| {
                    album.cover_away = false;
                    cx.notify();
                }),
                Hidden::Portrait(artist) => artist.update(cx, |artist, cx| {
                    artist.portrait_away = false;
                    cx.notify();
                }),
            }
        }
        cx.notify();
    }

    /// Says which page is open now, for a developer's trace.
    /// The frames of a change of page, once it has settled: how many, the
    /// longest gap between two and when it came, and every gap over 12 ms.
    fn trace_move(&mut self) {
        let frames = std::mem::take(&mut self.move_frames);
        let (Some(first), Some(last)) = (frames.first(), frames.last()) else {
            return;
        };
        let gaps: Vec<(f32, f32)> = frames
            .windows(2)
            .filter_map(|pair| match pair {
                [a, b] => Some((moves::since(*first, *a), moves::since(*a, *b))),
                _ => None,
            })
            .collect();
        let worst = gaps.iter().copied().fold(
            (0., 0.),
            |worst, gap| if gap.1 > worst.1 { gap } else { worst },
        );
        let long: Vec<String> = gaps
            .iter()
            .filter(|(_, gap)| *gap > 12.)
            .map(|(at, gap)| format!("{gap:.0} at +{at:.0}"))
            .collect();
        crate::debug(format_args!(
            "move: {} frames in {:.0} ms, longest gap {:.1} ms at +{:.0} ms; over 12 ms: [{}]",
            frames.len(),
            moves::since(*first, *last),
            worst.1,
            worst.0,
            long.join(", ")
        ));
    }

    fn traced(&self, how: &str, cx: &gpui::App) {
        let page = match self.pages.current() {
            Page::Library => format!("the {} wall", self.wall.read(cx).kind.name()),
            Page::Album { album, .. } => format!("album “{}”", album.read(cx).row.title),
            Page::Artist { artist, .. } => format!("artist {}", artist.read(cx).artist_id),
            Page::History(_) => "History".to_owned(),
            Page::NotBuilt(tab) => format!("{} (not built)", tab.label()),
        };
        crate::debug(format_args!(
            "page: {how} {page}{}",
            if self.arrival.hold {
                ", the banner held"
            } else {
                ""
            }
        ));
    }

    pub(crate) fn open_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let found = self.wall.update(cx, |wall, _| {
            Some((wall.row(index)?, wall.art_bounds(index)?))
        });
        if let Some((row, art)) = found {
            self.open(row, index, art, window, cx);
        }
    }

    /// Back: the page before, exactly as it was left. From an album, its
    /// cover travels home: to its tile on the wall, or its record on the
    /// page before.
    pub(crate) fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Out of Lean back, while it is open.
        if self.close_lean(cx) {
            return;
        }
        if !self.pages.can_go_back() {
            return;
        }
        self.finish_carry(cx);
        let left = self.shown();
        let left_top = left.top(cx);
        let came = self.album().and_then(|(_, from)| from.cloned());
        let leaving_artist = self.artist_page().map(|page| page.read(cx).artist_id);
        let back_to_wall = matches!(self.pages.previous(), Some(Page::Library));
        let mut carried = self.carry_cover_from(&left, cx);
        let flies_home = back_to_wall && self.carry_portrait_home(&left, window, cx);
        self.pages.back();
        // Back on the wall from an artist's page, theirs settles into place,
        // unless their picture flies home to it.
        if back_to_wall
            && !flies_home
            && let Some(artist) = leaving_artist
        {
            self.wall.update(cx, |wall, cx| wall.settle(artist, cx));
        }
        // Home to the tile it came from, which may not be the one the wall
        // would find first.
        if let (Some(carry), Some(Came::Wall(index))) = (carried.as_mut(), &came)
            && back_to_wall
        {
            let index = *index;
            self.wall.update(cx, |wall, _| wall.away = Some(index));
            carry.hidden.push(Hidden::Wall);
            carry.target = Target::Wall(index);
            self.carry = carried.take();
        }
        self.aim_carried(carried, &left, cx);
        self.moved(left, left_top, cx);
    }

    /// The page is ready: a cover waiting for it is aimed at where its
    /// target rests now, before the page hops (`stage.ready`).
    fn aim(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        let Some(carry) = &mut self.carry else { return };
        if !matches!(carry.phase, Phase::Waiting) {
            return;
        }
        let to = match &carry.target {
            Target::Album(album) => Some(
                album
                    .read(cx)
                    .cover_bounds(width - upnext::rail(cx), TITLE_BAR),
            ),
            Target::Wall(index) => self.wall.read(cx).art_bounds(*index),
            Target::Landing(landings, record) => landings.bounds(*record),
            Target::Portrait(artist) => artist
                .read(cx)
                .expects_portrait()
                .then(|| Self::portrait_bounds(width, 0.)),
            Target::Nowhere => None,
        };
        let now = Instant::now();
        carry.phase = match to.filter(|to| moves::seen(*to, TITLE_BAR, height - PLAYER_BAR)) {
            Some(to) => {
                carry.radius.1 = match carry.target {
                    Target::Album(_) => 12.,
                    _ => 10.,
                };
                Phase::Flying { start: now, to }
            }
            None => Phase::Dropping { start: now },
        };
    }
}

/// CSS's `cubic-bezier(x1, y1, x2, y2)` at `t`.
pub(crate) fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let curve = |a: f32, b: f32, s: f32| {
        3. * a * s * (1. - s).powi(2) + 3. * b * s * s * (1. - s) + s.powi(3)
    };
    // Solve x(s) = t by bisection; it is monotonic.
    let (mut low, mut high) = (0f32, 1f32);
    for _ in 0..24 {
        let mid = (low + high) / 2.;
        if curve(x1, x2, mid) < t {
            low = mid;
        } else {
            high = mid;
        }
    }
    curve(y1, y2, (low + high) / 2.)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn wash_layer(image: Arc<RenderImage>, opacity: f32, width: f32, height: f32) -> AnyElement {
    // The picture a little larger than the window, so its edges are never seen.
    let side = width.max(height) + 180.;
    let (left, top) = ((width - side) / 2., (height - side) / 2.);
    // In the light palette the real wash is a little lighter and fainter
    // (`opacity: 0.8`).
    let opacity = if theme::dark() {
        opacity
    } else {
        opacity * 0.8
    };
    let window = gpui::Bounds::new(
        gpui::point(px(0.), px(0.)),
        gpui::size(px(width), px(height)),
    );
    frost::note(
        gpui::Bounds::new(
            gpui::point(px(left), px(top)),
            gpui::size(px(side), px(side)),
        ),
        window,
        frost::Fill::Picture(Arc::clone(&image)),
        opacity,
    );
    img(image)
        .absolute()
        .left(px(left))
        .top(px(top))
        .size(px(side))
        .object_fit(ObjectFit::Cover)
        .opacity(opacity)
        .into_any_element()
}

/// The wash's own blur, `blur(64px)`, as a Gaussian's deviation in points.
const WASH_SIGMA: f32 = 64.;
/// The bands the fade at each edge is drawn in, and how far each reaches.
const FADE_BANDS: usize = 6;
const FADE_BAND: f32 = 32.;

/// How much of the wash is missing `d` points in from the window's edge.
/// WebKit blurs the real wash within the window, so the blur draws in
/// nothing from beyond each edge: half the wash at the edge itself, nearly
/// all of it three deviations in. GPUI's picture is blurred with its edges
/// carried outward, so the fade is laid over it in the window's colour,
/// which is what shows where the real wash thins.
fn missing(d: f32) -> f32 {
    // 1 − Φ(d/σ), by Abramowitz and Stegun's 7.1.26 for erf.
    let x = d / (WASH_SIGMA * std::f32::consts::SQRT_2);
    let t = 1. / (1. + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_6
            + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    let erf = 1. - poly * (-x * x).exp();
    0.5 * (1. - erf)
}

/// The wash's fade at the window's four edges (`missing`), in bands of the
/// window's colour: where two edges meet, two bands laid over each other
/// leave the product of what each lets through, as the real blur does.
fn edge_fade(width: f32, height: f32, bg: gpui::Rgba) -> Vec<AnyElement> {
    let with = |alpha: f32| gpui::Rgba { a: alpha, ..bg };
    let mut bands = Vec::new();
    for band in 0..FADE_BANDS {
        let near = band as f32 * FADE_BAND;
        let (inner, outer) = (missing(near + FADE_BAND), missing(near));
        // Top, bottom, left and right: where the band is, which way its
        // gradient runs, and its alpha at the start and end of that way.
        let edges = [
            (
                0.,
                near,
                width,
                FADE_BAND,
                frost::Toward::Down,
                outer,
                inner,
            ),
            (
                0.,
                height - near - FADE_BAND,
                width,
                FADE_BAND,
                frost::Toward::Down,
                inner,
                outer,
            ),
            (
                near,
                0.,
                FADE_BAND,
                height,
                frost::Toward::Right,
                outer,
                inner,
            ),
            (
                width - near - FADE_BAND,
                0.,
                FADE_BAND,
                height,
                frost::Toward::Right,
                inner,
                outer,
            ),
        ];
        for (x, y, w, h, toward, from, to) in edges {
            let rect = gpui::Bounds::new(gpui::point(px(x), px(y)), gpui::size(px(w), px(h)));
            let angle = if matches!(toward, frost::Toward::Down) {
                180.
            } else {
                90.
            };
            frost::note(
                rect,
                rect,
                frost::Fill::Gradient {
                    toward,
                    from: with(from),
                    to: with(to),
                    start: 0.,
                    end: 1.,
                },
                1.,
            );
            bands.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .h(px(h))
                    .bg(linear_gradient(
                        angle,
                        linear_color_stop(with(from), 0.),
                        linear_color_stop(with(to), 1.),
                    ))
                    .into_any_element(),
            );
        }
    }
    bands
}

/// The veil the wash is seen through (`frame/Wash.svelte`): the window's
/// colour, thinnest at the top, its four stops as three bands, as GPUI's
/// gradients have two. Each band's top and bottom as shares of the window's
/// height, and its alpha at each.
const VEIL: [(f32, f32, f32, f32); 3] = [
    (0., 0.38, 0.58, 0.74),
    (0.38, 0.72, 0.74, 0.90),
    (0.72, 1., 0.90, 0.95),
];

impl Shell {
    /// Up next's card in its place, flying in as it opens and out once it
    /// closes (Svelte's `fly`: 24 points from the right, fading, eased by
    /// `cubicOut`); with reduced motion it simply comes and goes. Whether
    /// it is still flying.
    fn card(
        &mut self,
        height: f32,
        washed: bool,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, bool) {
        let now = Instant::now();
        let still = crate::platform::reduce_motion();
        let open = self.player.read(cx).open;
        match &mut self.card {
            Some(card) if open && card.closing.is_some() => {
                self.card = None;
            }
            Some(card) if !open && card.closing.is_none() => {
                crate::debug(format_args!("up next: closed"));
                card.closing = Some(now);
            }
            _ => {}
        }
        if open && self.card.is_none() {
            let (link, player) = (self.link.clone(), self.player.clone());
            crate::debug(format_args!(
                "up next: open, {}",
                if self.pushed(cx) {
                    "moving the page over"
                } else {
                    "over the page"
                }
            ));
            self.card = Some(Card {
                card: cx.new(|cx| UpNextCard::new(link, player, cx)),
                opened: now,
                closing: None,
            });
        }
        let Some(card) = &self.card else {
            return (None, false);
        };
        let (shown, flying) = match card.closing {
            Some(at) => {
                let t = moves::progress(moves::since(at, now), upnext::FLY_OUT_MS);
                if still || t >= 1. {
                    self.card = None;
                    return (None, false);
                }
                (1. - upnext::cubic_out(t), true)
            }
            None if still => (1., false),
            None => {
                let t = moves::progress(moves::since(card.opened, now), upnext::FLY_IN_MS);
                (upnext::cubic_out(t), t < 1.)
            }
        };
        let over = !self.pushed(cx);
        card.card.update(cx, |card, _| {
            card.over = over;
            card.washed = washed;
        });
        let element = div()
            .absolute()
            .top(px(upnext::TOP))
            .right(px(upnext::RIGHT - upnext::FLY_X * (1. - shown)))
            .w(px(upnext::WIDTH))
            .h(px((height - upnext::TOP - upnext::BOTTOM).max(0.)))
            .opacity(shown)
            .child(card.card.clone())
            .into_any_element();
        (Some(element), flying)
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let size_now = window.viewport_size();
        let (width, height) = (f32::from(size_now.width), f32::from(size_now.height));
        self.width = width;
        // With room, Up next's card moves the page over: each page is laid
        // out in the window less the rail.
        let rail = if self.pushed(cx) { upnext::RAIL } else { 0. };
        if upnext::rail(cx) != rail {
            cx.set_global(upnext::Rail(rail));
        }
        let still = crate::platform::reduce_motion();
        let mut animating = false;
        let mut layers: Vec<AnyElement> = Vec::new();
        let shown = self.shown();
        // Up next's card frosts what is behind it: what is drawn is noted
        // for it, from the window's colour up, while it is shown.
        let frosted = self.card.is_some() || self.player.read(cx).open;
        frost::begin(frosted);
        let whole = gpui::Bounds::new(
            gpui::point(px(0.), px(0.)),
            gpui::size(px(width), px(height)),
        );
        frost::note(whole, whole, frost::Fill::Solid(theme::bg()), 1.);

        // The page that arrives is shown once it is ready, or after a
        // quarter of a second as it is; a cover waiting for it is aimed then.
        if self.arrival.ready.is_none()
            && (shown.ready(cx) || now.duration_since(self.arrival.started) >= moves::READY_WITHIN)
        {
            crate::debug(format_args!(
                "page: shown after {:.0} ms{}",
                moves::since(self.arrival.started, now),
                if shown.ready(cx) { "" } else { ", not ready" }
            ));
            self.arrival.ready = Some(now);
            self.aim(width, height, cx);
        }

        // The wash: the colours the pages borrowed, the last over the ones
        // before, from edge to edge behind the title bar, the page and the
        // player bar, under a veil of the window's colour that is thinnest
        // at the top. A new colour comes up over the last; the same stays;
        // a page with none lets them ebb away.
        if shown.bannered()
            && let Some((key, image)) = shown.wash(cx)
            && self.tints.last().is_none_or(|top| top.key != key)
        {
            let since = if still {
                now.checked_sub(std::time::Duration::from_secs(1))
                    .unwrap_or(now)
            } else {
                now
            };
            self.tints.push(Tint { key, image, since });
            self.ebbing = None;
        }
        if shown.bannered() {
            self.ebbing = None;
        }
        // The colours under the last go once it is up.
        if let Some(top) = self.tints.last()
            && moves::since(top.since, now) >= moves::TINT_MS
            && self.tints.len() > 1
        {
            let last = self.tints.len() - 1;
            self.tints.drain(..last);
        }
        let ebb = match self.ebbing {
            Some(at) if still => {
                let _ = at;
                self.tints.clear();
                self.ebbing = None;
                0.
            }
            Some(at) => {
                let t = moves::progress(moves::since(at, now), moves::EBB_MS);
                if t >= 1. {
                    self.tints.clear();
                    self.ebbing = None;
                }
                1. - moves::ease_out(t)
            }
            None => 1.,
        };
        let washed = !self.tints.is_empty();
        let top = self.tints.len().saturating_sub(1);
        for (at, tint) in self.tints.iter().enumerate() {
            let up = if at == top {
                moves::ease(moves::progress(
                    moves::since(tint.since, now),
                    moves::TINT_MS,
                ))
            } else {
                1.
            };
            animating |= up < 1.;
            layers.push(wash_layer(Arc::clone(&tint.image), up * ebb, width, height));
        }
        animating |= self.ebbing.is_some();
        if washed {
            let bg = theme::bg();
            let with = |alpha: f32| gpui::Rgba { a: alpha, ..bg };
            layers.extend(edge_fade(width, height, bg));
            for (from, to, alpha_from, alpha_to) in VEIL {
                let (top, band) = (height * from, height * (to - from));
                let rect = gpui::Bounds::new(
                    gpui::point(px(0.), px(top)),
                    gpui::size(px(width), px(band)),
                );
                frost::note(
                    rect,
                    rect,
                    frost::Fill::Gradient {
                        toward: frost::Toward::Down,
                        from: with(alpha_from),
                        to: with(alpha_to),
                        start: 0.,
                        end: 1.,
                    },
                    1.,
                );
                layers.push(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(top))
                        .w_full()
                        .h(px(band))
                        .bg(linear_gradient(
                            180.,
                            linear_color_stop(with(alpha_from), 0.),
                            linear_color_stop(with(alpha_to), 1.),
                        ))
                        .into_any_element(),
                );
            }
        }

        // The page left, fading out under the one that arrives: all of it,
        // or with the banner held, its body alone.
        let mut pages: Vec<AnyElement> = Vec::new();
        if let Some((left, leaving)) = self.leaving.clone() {
            match leaving.at(now) {
                Some(opacity) => {
                    animating = true;
                    let whole = if leaving.hold {
                        left.look(
                            moves::Look {
                                body: opacity,
                                ..moves::Look::default()
                            },
                            cx,
                        );
                        1.
                    } else {
                        opacity
                    };
                    pages.push(
                        div()
                            .absolute()
                            .size_full()
                            .opacity(whole)
                            .child(frost::faded(whole, left.element(&self.wall, height)))
                            .into_any_element(),
                    );
                }
                None => {
                    left.look(moves::Look::default(), cx);
                    self.leaving = None;
                }
            }
        }
        // The page that arrives: unseen until it is ready, then a short hop
        // up as it fades in, its banner holding still or making its entrance.
        let (opacity, hop, look) = self.arrival.at(now);
        animating |= self.arrival.moving(now);
        shown.look(look, cx);
        pages.push(
            div()
                .absolute()
                .size_full()
                .top(px(hop))
                .opacity(opacity)
                .child(frost::faded(opacity, shown.element(&self.wall, height)))
                .into_any_element(),
        );
        // Under Lean back, while it covers the window whole, the page is not drawn.
        let covered = self
            .lean
            .as_ref()
            .is_some_and(|(lean, _)| lean.read(cx).covers());
        if covered {
            pages.clear();
        }
        let content = div()
            .relative()
            .size_full()
            .children(pages)
            .into_any_element();
        let server = self.link.read(cx).net().map(|net| {
            let away = crate::away::said(&net);
            let clock = crate::away::clock();
            let card = self.connection_hovered.then(|| match (&away, &net.hub) {
                (Some(away), _) => Some(away.card(&clock)),
                (None, Some(hub)) => Some(crate::away::fine_card(&hub.name)),
                (None, None) => None,
            });
            let shell = cx.entity().downgrade();
            frame::Server {
                // On a satellite, the computer the library is on.
                name: net
                    .hub
                    .as_ref()
                    .map_or_else(|| net.name.clone(), |hub| hub.name.clone())
                    .into(),
                away: away.is_some(),
                card: card
                    .flatten()
                    .map(|(first, then)| (first.into(), then.into())),
                on_hover: std::rc::Rc::new(move |over, _, cx| {
                    let _ = shell.update(cx, |shell, cx| shell.hover_connection(over, cx));
                }),
            }
        });
        let shell = cx.entity().downgrade();
        let on_tab: frame::OnTab = std::rc::Rc::new(move |tab, _, cx| {
            let _ = shell.update(cx, |shell, cx| shell.go(tab, cx));
        });

        // The cover on its way: waiting where it was clicked, flying, then
        // holding where it lands until the page has stopped moving; or with
        // nowhere to land, fading where it stands.
        let mut flying: Option<AnyElement> = None;
        let settled = self.arrival.settled_at();
        let mut done = false;
        // Where it is going, read again each frame: the wall lays itself out
        // again on the frame it comes back, after the cover was aimed, and
        // a window resized meanwhile moves the banner's picture.
        let retarget = match self.carry.as_ref().map(|carry| &carry.target) {
            Some(Target::Wall(index)) => self.wall.read(cx).art_bounds(*index),
            Some(Target::Portrait(_)) => Some(Self::portrait_bounds(width, 0.)),
            _ => None,
        };
        if let Some(carry) = &mut self.carry {
            if let (Some(bounds), Phase::Flying { to, .. } | Phase::Holding { to }) =
                (retarget, &mut carry.phase)
            {
                *to = bounds;
            }
            carry.frames.push(now);
            animating = true;
            let (bounds, opacity, ended) = moves::flight_at(carry.phase, carry.from, now);
            if ended {
                match carry.phase {
                    Phase::Flying { to, .. } => carry.phase = Phase::Holding { to },
                    _ => done = true,
                }
            }
            if let Phase::Holding { .. } = carry.phase
                && settled.is_none_or(|settled| now >= settled)
            {
                done = true;
            }
            let along = match carry.phase {
                Phase::Flying { start, .. } => {
                    moves::fly_curve(moves::progress(moves::since(start, now), moves::FLY_MS))
                }
                Phase::Holding { .. } => 1.,
                _ => 0.,
            };
            if carry.round != (false, false) {
                // An artist's picture: each end drawn as it is drawn there,
                // the one it flies to coming in over the one it leaves
                // before that goes (`Flight`), and never reshaped on the way.
                let to_image = carry.to_image.clone().or_else(|| match &carry.target {
                    Target::Portrait(artist) => artist.read(cx).portrait(),
                    _ => None,
                });
                let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let face = |image: Option<Arc<RenderImage>>, round: bool, shown: f32| {
                    if round {
                        let d = w.min(h);
                        div()
                            .absolute()
                            .left(px((w - d) / 2.))
                            .top(px((h - d) / 2.))
                            .size(px(d))
                            .rounded_full()
                            .overflow_hidden()
                            .opacity(shown)
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(carry.ground.0, 0.),
                                linear_color_stop(carry.ground.1, 1.),
                            ))
                            .children(image.map(|image| img(image).size(px(d)).rounded_full()))
                    } else {
                        // At its own shape, as tall as the box and centred
                        // across it, as the round one is.
                        let shape = image.as_ref().map_or(w / h.max(1.), |image| {
                            let size = image.size(0);
                            size.width.0.max(1) as f32 / size.height.0.max(1) as f32
                        });
                        let across = h * shape;
                        div()
                            .absolute()
                            .top_0()
                            .left(px((w - across) / 2.))
                            .w(px(across))
                            .h(px(h))
                            .opacity(shown)
                            .children(image.map(|image| {
                                img(image).size_full().object_fit(gpui::ObjectFit::Fill)
                            }))
                    }
                };
                let (leaving, coming) = moves::crossed(along);
                flying = Some(
                    div()
                        .absolute()
                        .left(bounds.origin.x)
                        .top(bounds.origin.y)
                        .w(px(w))
                        .h(px(h))
                        .opacity(opacity)
                        .child(face(carry.image.clone(), carry.round.0, leaving))
                        .child(face(to_image, carry.round.1, coming))
                        .into_any_element(),
                );
            } else {
                let radius = lerp(carry.radius.0, carry.radius.1, along);
                let side = f32::from(bounds.size.width);
                let mut cover = div()
                    .absolute()
                    .left(bounds.origin.x)
                    .top(bounds.origin.y)
                    .size(px(side))
                    .rounded(px(radius))
                    .opacity(opacity)
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(carry.ground.0, 0.),
                        linear_color_stop(carry.ground.1, 1.),
                    ));
                if let Some(image) = &carry.image {
                    cover = cover.child(
                        img(Arc::clone(image))
                            .absolute()
                            .top_0()
                            .left_0()
                            .size(px(side))
                            .rounded(px(radius)),
                    );
                }
                flying = Some(cover.into_any_element());
            }
        }
        if done {
            self.finish_carry(cx);
            flying = None;
        }
        if !self.move_frames.is_empty() {
            self.move_frames.push(now);
            if !animating {
                self.trace_move();
            }
        }
        if animating {
            window.request_animation_frame();
        }
        self.bar.update(cx, |bar, _| bar.washed = washed);
        let (card, flying_card) = self.card(height, washed, cx);
        if card.is_none() {
            frost::forget(window);
        }
        if flying_card {
            window.request_animation_frame();
        }
        let toast = self
            .player
            .read(cx)
            .toast
            .clone()
            .map(|(text, since)| crate::bar::toast(text, since, width, window));

        div()
            .id("shell")
            .track_focus(&self.focus)
            .key_context("Ritornel")
            .on_action(cx.listener(|shell, _: &Back, window, cx| shell.back(window, cx)))
            // Not under Lean back, on its way out included (`App.svelte`).
            .on_action(cx.listener(|shell, _: &crate::frame::ToggleUpNext, _, cx| {
                if shell.lean.is_none() {
                    shell.toggle_card(None, cx);
                }
            }))
            .on_action(cx.listener(|shell, _: &crate::frame::Find, window, cx| {
                // Search is in the title bar, under Lean back.
                shell.close_lean(cx);
                shell
                    .search
                    .update(cx, |search, cx| search.find(window, cx));
            }))
            .on_action(
                cx.listener(|shell, _: &crate::frame::LeanBack, window, cx| {
                    shell.toggle_lean(window, cx);
                }),
            )
            // Keys for the page on screen: the Library's wall takes letters,
            // the arrows, Return and Esc (`lib/keys.ts`).
            .on_key_down(
                cx.listener(|shell, event: &gpui::KeyDownEvent, window, cx| {
                    // Typing in the search field is the field's.
                    if shell.search.read(cx).focused(window, cx) {
                        return;
                    }
                    let keys = &event.keystroke;
                    let mods = keys.modifiers;
                    // With Lean back open there is no wall: Esc puts it
                    // away, and ← and → turn its facts.
                    if let Some((lean, _)) = &shell.lean {
                        let bare = !mods.platform && !mods.control && !mods.alt;
                        let by = match keys.key.as_str() {
                            "left" if bare => Some(-1),
                            "right" if bare => Some(1),
                            _ => None,
                        };
                        if keys.key == "escape" && bare && !mods.shift {
                            lean.update(cx, |lean, cx| lean.close(cx));
                            cx.stop_propagation();
                            return;
                        }
                        if let Some(by) = by {
                            lean.update(cx, |lean, cx| lean.key_step(by, cx));
                            cx.stop_propagation();
                            return;
                        }
                    }
                    if shell.lean.is_none()
                        && matches!(shell.pages.current(), Page::Library)
                        && shell
                            .wall
                            .update(cx, |wall, cx| wall.key(event, window, cx))
                    {
                        cx.stop_propagation();
                        return;
                    }
                    // Space plays or pauses, and ⌥← and ⌥→ go to the
                    // previous and next song (Ctrl+← and Ctrl+→ off a
                    // Mac: `platform::KEYS`). Esc puts Up next away.
                    let keys = &event.keystroke;
                    let mods = keys.modifiers;
                    let bare = !mods.platform && !mods.control && !mods.function;
                    if keys.key == "escape" && bare && !mods.alt && shell.player.read(cx).open {
                        shell.toggle_card(Some(false), cx);
                        cx.stop_propagation();
                        return;
                    }
                    let action = match keys.key.as_str() {
                        "space" if bare && !mods.alt && !mods.shift => Some(None),
                        "left" if mods == crate::platform::KEYS.skip => {
                            Some(Some(SkipDirection::Previous))
                        }
                        "right" if mods == crate::platform::KEYS.skip => {
                            Some(Some(SkipDirection::Next))
                        }
                        _ => None,
                    };
                    if let Some(skip) = action {
                        shell.player.update(cx, |player, cx| match skip {
                            None => player.toggle(cx),
                            Some(direction) => player.skip(direction, cx),
                        });
                        cx.stop_propagation();
                    }
                }),
            )
            // A two-finger swipe to the right is Back (`swipe.rs`).
            .on_scroll(cx.listener(|shell, _: &gpui::ScrollEvent, window, cx| {
                if crate::platform::swiped_back() {
                    shell.back(window, cx);
                }
            }))
            // The mouse's back button, as in the real app.
            .on_mouse_down(
                gpui::MouseButton::Navigate(gpui::NavigationDirection::Back),
                cx.listener(|shell, _, window, cx| shell.back(window, cx)),
            )
            .relative()
            .size_full()
            .bg(theme::bg())
            .font_family(".SystemUIFont")
            // The real page's body: 14 points on a line of 1.4. GPUI's own
            // line height is taller, and every block of text grows with it.
            .text_size(px(14.))
            .line_height(gpui::relative(1.4))
            .text_color(theme::ink())
            .overflow_hidden()
            .children(layers)
            .child(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(frame::title_bar(
                        self.pages.can_go_back(),
                        self.pages.current().tab(),
                        server,
                        self.search.clone().into_any_element(),
                        cx.listener(|shell, _, window, cx| shell.back(window, cx)),
                        on_tab,
                    ))
                    .child(div().flex_1().min_h_0().child(content))
                    .child(self.bar.clone()),
            )
            // A picture on its way goes under Up next's card, as the banner's
            // picture it lands on or leaves does.
            .children(flying)
            .children(card)
            .children(self.lean.as_ref().map(|(lean, _)| lean.clone()))
            .children(toast)
            .children(crate::drawn::layer(window, cx))
    }
}

#[cfg(test)]
mod fade_tests {
    use super::missing;

    #[test]
    fn half_the_wash_is_missing_at_the_edge_and_none_three_deviations_in() {
        assert!((missing(0.) - 0.5).abs() < 1e-3);
        assert!((missing(64.) - 0.158_7).abs() < 1e-3);
        assert!((missing(128.) - 0.022_75).abs() < 1e-3);
        assert!(missing(192.) < 0.002);
    }
}
