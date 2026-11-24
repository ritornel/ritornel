//! Search in the title bar: what was typed, the results that drop down under
//! the field, which of them is chosen, and what choosing one does. The
//! server ranks each kind (`library.search`); this keeps the answers in
//! step with the typing and the keys.
//!
//! The results are artists, then records, then songs, a few of each. The
//! lit one starts at the kind the server says matched best, so that
//! "nocturne" lights the song Nocturne and not the album Smoke Nocturne
//! above it. Return before the answer has come asks at once and chooses its
//! first. A record's cover travels from the list to its page; a song opens
//! its album with the song marked, or plays when it is on no record.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext, Bounds, BoxShadow, Context, Entity, EventEmitter, FocusHandle,
    FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, ObjectFit, ParentElement,
    Pixels, Render, RenderImage, SharedString, StatefulInteractiveElement, Styled, StyledImage,
    Subscription, Task, Window, canvas, deferred, div, img, prelude::FluentBuilder, px,
};
use ritornel_contract::methods::LibrarySearch;
use ritornel_contract::{
    AlbumTile, ArtistRow, Id, ListKind, SearchParams, SearchResults, SongFound,
};

use crate::icons::icon;
use crate::input::{Field, FieldEvent};
use crate::link::Link;
use crate::theme;
use crate::wall::TileRow;

/// How long typing pauses before the library is asked.
const SETTLE: Duration = Duration::from_millis(60);
/// How many results of each kind appear in the dropdown.
const ARTISTS: usize = 3;
const ALBUMS: usize = 4;
const SONGS: usize = 5;
/// Most search-result thumbnails kept at once (four full dropdowns).
const MAX_PICTURES: usize = 48;

/// Bounded LRU thumbnail cache for search results that protects active hits.
#[derive(Default)]
struct Pictures {
    map: HashMap<Id, (Arc<RenderImage>, u64)>,
    in_flight: HashSet<Id>,
    tick: u64,
}

impl Pictures {
    fn get(&mut self, art: &Id) -> Option<Arc<RenderImage>> {
        self.tick += 1;
        let (image, used) = self.map.get_mut(art)?;
        *used = self.tick;
        Some(Arc::clone(image))
    }

    fn want(&mut self, art: Id) -> bool {
        !self.map.contains_key(&art) && self.in_flight.insert(art)
    }

    fn finish_failed(&mut self, art: Id) {
        self.in_flight.remove(&art);
    }

    fn clear_in_flight(&mut self) {
        self.in_flight.clear();
    }

    fn insert(
        &mut self,
        art: Id,
        image: Arc<RenderImage>,
        keep: &HashSet<Id>,
    ) -> Vec<Arc<RenderImage>> {
        self.in_flight.remove(&art);
        self.tick += 1;
        let mut evicted = Vec::new();
        if let Some((old, _)) = self.map.insert(art, (image, self.tick)) {
            evicted.push(old);
        }
        while self.map.len() > MAX_PICTURES {
            let oldest = self
                .map
                .iter()
                .filter(|(id, _)| !keep.contains(id))
                .min_by_key(|(_, (_, used))| *used)
                .map(|(id, _)| *id);
            let Some(old_id) = oldest else {
                break;
            };
            if let Some((old, _)) = self.map.remove(&old_id) {
                evicted.push(old);
            }
        }
        evicted
    }

    fn drain(&mut self) -> Vec<Arc<RenderImage>> {
        self.in_flight.clear();
        self.map.drain().map(|(_, (image, _))| image).collect()
    }
}

/// One result as it drops down.
#[derive(Debug, Clone)]
pub(crate) enum Hit {
    Artist(ArtistRow),
    Album(AlbumTile),
    Song(SongFound),
}

/// The results as they drop down: artists, then records, then songs.
pub(crate) fn hits_of(results: &SearchResults) -> Vec<Hit> {
    let artists = results
        .artists
        .iter()
        .take(ARTISTS)
        .cloned()
        .map(Hit::Artist);
    let albums = results.albums.iter().take(ALBUMS).cloned().map(Hit::Album);
    let songs = results.songs.iter().take(SONGS).cloned().map(Hit::Song);
    artists.chain(albums).chain(songs).collect()
}

/// Where the lit row starts: the first of the kind that matched best.
pub(crate) fn top_of(hits: &[Hit], results: &SearchResults) -> usize {
    let found = hits.iter().position(|hit| {
        matches!(
            (hit, results.top),
            (Hit::Artist(_), Some(ListKind::Artists))
                | (Hit::Album(_), Some(ListKind::Albums))
                | (Hit::Song(_), Some(ListKind::Songs))
        )
    });
    found.unwrap_or(0)
}

/// What choosing a result does.
pub(crate) enum SearchEvent {
    OpenArtist(Id, SharedString),
    /// A record, its cover lifted from the list; or a song's album, the
    /// song marked.
    OpenAlbum {
        row: Arc<TileRow>,
        lift: Option<(Bounds<Pixels>, Option<Arc<RenderImage>>)>,
        mark: Option<Id>,
    },
    /// A song on no record: it plays.
    Play(Id),
}

/// What the field and its dropdown show.
#[derive(Debug, Clone, Default)]
struct View {
    text: String,
    /// Whether anything was typed, and so whether the dropdown is down.
    open: bool,
    hits: Vec<Hit>,
    /// The chosen result: the first, until the arrows move it.
    selected: usize,
    /// Whether `hits` answer `text`.
    settled: bool,
    failed: bool,
    sent: bool,
    waiting: bool,
}

impl View {
    fn dispatch(&mut self) -> bool {
        !self.settled && !std::mem::replace(&mut self.sent, true)
    }

    fn fail(&mut self) {
        self.hits.clear();
        self.selected = 0;
        self.settled = true;
        self.failed = true;
        self.waiting = false;
    }

    fn start(&mut self, text: String) {
        self.text = text;
        self.open = true;
        self.selected = 0;
        self.settled = false;
        self.failed = false;
        self.sent = false;
        self.waiting = false;
    }
}

pub(crate) struct SearchBox {
    link: Entity<Link>,
    pub field: Entity<Field>,
    view: View,
    /// Bumped by every keystroke: an answer for an older one is dropped.
    asked: u64,
    asking: Option<Task<()>>,
    /// When the dropdown came down, for it to rise into place.
    dropped: Option<Instant>,
    pictures: Pictures,
    /// In-flight thumbnail fetches, aborted when cleared or dropped.
    fetches: crate::covers::FetchBag,
    /// Generation bumped whenever thumbnail fetches are cleared so late
    /// cancellation callbacks cannot erase a reopened search's bookkeeping.
    fetch_gen: u64,
    /// Where each row's picture is, for a record's cover to travel from.
    arts: Rc<std::cell::RefCell<HashMap<Id, Bounds<Pixels>>>>,
    /// Where the dropdown is, so a press in it does not close it.
    drop_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    hovered_song: Option<usize>,
    /// What has the keys when the field lets them go: the window's own
    /// keys, so that its shortcuts work again.
    home: FocusHandle,
    _field: Subscription,
}

impl EventEmitter<SearchEvent> for SearchBox {}

impl SearchBox {
    pub(crate) fn new(
        link: Entity<Link>,
        home: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = cx.new(|cx| Field::new("Search", cx));
        let followed = cx.subscribe_in(
            &field,
            window,
            |search, _, event: &FieldEvent, window, cx| match event {
                FieldEvent::Changed(text) => search.typed(text.to_string(), window, cx),
                FieldEvent::Moved(by) => search.move_by(*by, cx),
                FieldEvent::Entered => search.enter(window, cx),
                FieldEvent::Escaped => search.close(window, cx),
            },
        );
        cx.on_release(|search, cx| {
            for image in search.pictures.drain() {
                crate::covers::drop_unshared(image, None, cx);
            }
        })
        .detach();
        Self {
            link,
            field,
            view: View {
                settled: true,
                ..View::default()
            },
            asked: 0,
            asking: None,
            dropped: None,
            pictures: Pictures::default(),
            fetches: crate::covers::FetchBag::default(),
            fetch_gen: 0,
            arts: Rc::default(),
            drop_bounds: Rc::default(),
            hovered_song: None,
            home,
            _field: followed,
        }
    }

    /// Whether the field has the keys: then the window's own keys are not its.
    pub(crate) fn focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.field.read(cx).focused(window)
    }

    /// ⌘F, from anywhere: to the field, with what is in it selected.
    pub(crate) fn find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.field
            .update(cx, |field, cx| field.focus_all(window, cx));
    }

    /// The field's text changed.
    fn typed(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.asked += 1;
        self.hovered_song = None;
        if text.trim().is_empty() {
            self.asking = None;
            self.fetch_gen = self.fetch_gen.wrapping_add(1);
            self.fetches.clear();
            self.pictures.clear_in_flight();
            self.view = View {
                settled: true,
                ..View::default()
            };
            self.dropped = None;
            cx.notify();
            return;
        }
        if !self.view.open {
            self.dropped = Some(Instant::now());
        }
        self.view.start(text.clone());
        let asked = self.asked;
        self.asking = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            this.update_in(cx, |search, window, cx| search.ask(text, asked, window, cx))
                .ok();
        }));
        cx.notify();
    }

    fn ask(&mut self, text: String, asked: u64, window: &mut Window, cx: &mut Context<Self>) {
        if asked != self.asked || !self.view.dispatch() {
            return;
        }
        let Some(net) = self.link.read(cx).net() else {
            self.view.fail();
            cx.notify();
            return;
        };
        let client = Arc::clone(&net.client);
        let query = text.clone();
        let answer = net.rt().spawn(async move {
            client
                .call::<LibrarySearch>(&SearchParams {
                    text: query,
                    limit: Some(SONGS.max(ALBUMS).max(ARTISTS) as u32),
                })
                .await
        });
        self.asking = Some(cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            this.update_in(cx, |search, window, cx| {
                if asked != search.asked {
                    return;
                }
                match answer {
                    Ok(Ok(results)) => {
                        let hits = hits_of(&results);
                        let selected = top_of(&hits, &results);
                        let waiting = search.view.waiting;
                        search.view = View {
                            text,
                            open: true,
                            hits,
                            selected,
                            settled: true,
                            failed: false,
                            sent: true,
                            waiting: false,
                        };
                        search.pictures_wanted(cx);
                        if waiting {
                            search.pick(selected, window, cx);
                        }
                    }
                    _ => {
                        search.view.fail();
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The pictures of the rows, at 36 points.
    fn pictures_wanted(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.link.read(cx).net() else {
            return;
        };
        let arts: Vec<Id> = self
            .view
            .hits
            .iter()
            .filter_map(|hit| match hit {
                Hit::Artist(row) => row.art_id,
                Hit::Album(tile) => tile.art_id,
                Hit::Song(song) => song.art_id,
            })
            .collect();
        for art in arts {
            if !self.pictures.want(art) {
                continue;
            }
            let px = 72;
            let generation = self.fetch_gen;
            let http = Arc::clone(&net.art);
            let sizes = net.art_sizes.clone();
            let read = net
                .rt()
                .spawn(async move { crate::covers::fetch_square(&http, &sizes, art, px).await });
            self.fetches.push(read.abort_handle());
            cx.spawn(async move |this, cx| {
                let Ok(result) = read.await else {
                    return;
                };
                this.update(cx, |search, cx| {
                    if search.fetch_gen != generation {
                        return;
                    }
                    match result {
                        Ok((image, _, _)) => {
                            let keep: HashSet<Id> = search
                                .view
                                .hits
                                .iter()
                                .filter_map(|hit| match hit {
                                    Hit::Artist(row) => row.art_id,
                                    Hit::Album(tile) => tile.art_id,
                                    Hit::Song(song) => song.art_id,
                                })
                                .collect();
                            for old in search.pictures.insert(art, Arc::new(image), &keep) {
                                crate::covers::drop_unshared(old, None, cx);
                            }
                            cx.notify();
                        }
                        Err(_) => search.pictures.finish_failed(art),
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// The arrows: moves the choice, staying within the results.
    fn move_by(&mut self, by: i32, cx: &mut Context<Self>) {
        if !self.view.settled {
            return;
        }
        let Some(last) = self.view.hits.len().checked_sub(1) else {
            return;
        };
        let at = self.view.selected as i64 + i64::from(by);
        self.view.selected = at.clamp(0, last as i64) as usize;
        cx.notify();
    }

    /// Return: chooses the chosen result; typed faster than the settle, it
    /// asks at once and chooses the first of the answer.
    fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.view.open {
            return;
        }
        if self.view.settled {
            self.pick(self.view.selected, window, cx);
            return;
        }
        self.view.waiting = true;
        let (text, asked) = (self.view.text.clone(), self.asked);
        self.ask(text, asked, window, cx);
    }

    /// Chooses the result at `index`, and the dropdown closes.
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.view.hits.get(index).cloned() else {
            return;
        };
        let event = match hit {
            Hit::Artist(row) => {
                SearchEvent::OpenArtist(row.artist_id, row.name.unwrap_or_default().into())
            }
            Hit::Album(tile) => {
                let id = tile.album.id();
                let image = tile.art_id.and_then(|art| self.pictures.get(&art));
                let lift = self.arts.borrow().get(&id).copied().map(|at| (at, image));
                SearchEvent::OpenAlbum {
                    row: Arc::new(TileRow::of_album(tile)),
                    lift,
                    mark: None,
                }
            }
            Hit::Song(found) => match found.song.release_id {
                None => SearchEvent::Play(found.song.appearance_id),
                Some(release) => SearchEvent::OpenAlbum {
                    row: Arc::new(TileRow::of_album(AlbumTile {
                        album: ritornel_contract::AlbumRef::Release { id: release },
                        title: found.song.album_title.clone(),
                        artist_credit: found.song.artist_credit.clone(),
                        year: found.song.year,
                        track_count: 0,
                        duration_ms: 0,
                        version_count: 0,
                        art_id: found.art_id,
                        quality: None,
                    })),
                    lift: None,
                    mark: Some(found.song.appearance_id),
                },
            },
        };
        self.close(window, cx);
        cx.emit(event);
    }

    /// Esc, a press elsewhere, or a choice: the text goes, and the dropdown
    /// with it. If the field still has the keys, they go back to the
    /// window's own. A press elsewhere closes this before it reaches what
    /// it pressed, so a control that takes the keys on a press still takes
    /// them after.
    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.asked += 1;
        self.hovered_song = None;
        self.asking = None;
        self.fetch_gen = self.fetch_gen.wrapping_add(1);
        self.fetches.clear();
        self.pictures.clear_in_flight();
        self.view = View {
            settled: true,
            ..View::default()
        };
        self.dropped = None;
        self.arts.borrow_mut().clear();
        self.field.update(cx, |field, cx| field.clear(window, cx));
        if self.focused(window, cx) {
            window.focus(&self.home, cx);
        }
        cx.notify();
    }
}

/// A heading in the dropdown, and which results are under it.
type Group = (&'static str, fn(&Hit) -> bool);

/// An artist with no picture: their initial, as the walls draw it.
fn initial(name: &str) -> String {
    let filed = ["the ", "a ", "an "]
        .iter()
        .find_map(|article| {
            name.get(..article.len())
                .filter(|start| start.eq_ignore_ascii_case(article))
                .map(|_| name.get(article.len()..).unwrap_or(name))
        })
        .unwrap_or(name);
    filed
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect())
}

impl SearchBox {
    /// A row's words, its picture's key, and whether it is round.
    fn shown(hit: &Hit) -> (String, String, Option<Id>, Id, bool) {
        match hit {
            Hit::Artist(row) => (
                row.name
                    .clone()
                    .unwrap_or_else(|| "Unknown artist".to_owned()),
                crate::words::plural(row.album_count, "album"),
                row.art_id,
                row.artist_id,
                true,
            ),
            Hit::Album(tile) => (
                tile.title.clone().unwrap_or_else(|| "Untitled".to_owned()),
                [
                    Some(
                        tile.artist_credit
                            .as_ref()
                            .map_or_else(|| "Unknown artist".to_owned(), |c| c.text.clone()),
                    ),
                    tile.year.map(|year| year.to_string()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
                tile.art_id,
                tile.album.id(),
                false,
            ),
            Hit::Song(found) => (
                found
                    .song
                    .title
                    .clone()
                    .unwrap_or_else(|| "Untitled".to_owned()),
                [
                    Some(
                        found
                            .song
                            .artist_credit
                            .as_ref()
                            .map_or_else(|| "Unknown artist".to_owned(), |c| c.text.clone()),
                    ),
                    found.song.album_title.clone(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
                found.art_id,
                found.song.appearance_id,
                false,
            ),
        }
    }

    /// The dropdown: a raised card 440 points wide at the field's right
    /// edge, its rows 50 points with a 36-point picture in each.
    fn dropdown(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.view.open || (!self.view.settled && self.view.hits.is_empty()) {
            self.drop_bounds.set(None);
            return None;
        }
        let size = window.viewport_size();
        let (width, height) = (f32::from(size.width), f32::from(size.height));
        let wide = 440f32.min(width - 32.);
        // It rises into place as it comes down.
        let risen = if crate::platform::reduce_motion() {
            1.
        } else {
            self.dropped.map_or(1., |at| {
                crate::moves::progress(crate::moves::since(at, Instant::now()), 140.)
            })
        };
        if risen < 1. {
            window.request_animation_frame();
        }
        let mut card = div()
            .id("search-results")
            .absolute()
            // 46 below the window's top and 16 in from its right, as the
            // real one sits: the field is 10 down and ends 16 in.
            .top(px(36. + 6. * (1. - risen)))
            .right_0()
            .w(px(wide))
            .max_h(px(620f32.min(height - 140.)))
            .overflow_y_scroll()
            .p(px(6.))
            .rounded(px(14.))
            .bg(theme::raised())
            .text_size(px(14.))
            .text_color(theme::ink())
            .opacity(risen)
            .shadow(shadow())
            // A press in the list leaves the keys in the field.
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            });
        if self.view.failed {
            card = card.child(empty("The library could not be searched."));
        } else if self.view.hits.is_empty() {
            card = card.child(empty("Nothing in your library matches."));
        }
        let groups: [Group; 3] = [
            ("Artists", |hit| matches!(hit, Hit::Artist(_))),
            ("Albums", |hit| matches!(hit, Hit::Album(_))),
            ("Songs", |hit| matches!(hit, Hit::Song(_))),
        ];
        let hits = self.view.hits.clone();
        for (label, of) in groups {
            let members: Vec<(usize, &Hit)> =
                hits.iter().enumerate().filter(|(_, hit)| of(hit)).collect();
            if members.is_empty() {
                continue;
            }
            card = card.child(
                div()
                    .pt(px(10.))
                    .px(px(10.))
                    .pb(px(4.))
                    .text_size(px(12.))
                    .font_weight(FontWeight(600.))
                    .text_color(theme::dim())
                    .child(label),
            );
            for (index, hit) in members {
                card = card.child(self.row(index, hit, window, cx));
            }
        }
        // Where the whole card is, for a press in it not to close it: from
        // its top left, not after its rows, where it would otherwise sit.
        let bounds = Rc::clone(&self.drop_bounds);
        card = card.child(
            canvas(move |at, _, _| bounds.set(Some(at)), |_, (), _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        );
        Some(deferred(card).with_priority(1).into_any_element())
    }

    fn row(
        &mut self,
        index: usize,
        hit: &Hit,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, sub, art, key, round) = Self::shown(hit);
        let asked = self.asked;
        let settled = self.view.settled;
        let lit = settled && index == self.view.selected;
        let picture = art.and_then(|art| self.pictures.get(&art));
        let scale = window.scale_factor();
        let soft = (art.is_none())
            .then(|| theme::soft_cached(&format!("{title}{sub}"), (36. * scale).ceil() as u32));
        let song = matches!(hit, Hit::Song(_));
        let playing = settled && self.hovered_song == Some(index) && song;
        let arts = Rc::clone(&self.arts);
        let radius = if round { 18. } else { 6. };
        let mut face = div()
            .relative()
            .size(px(36.))
            .flex_none()
            .rounded(px(radius))
            .bg(theme::fill())
            .flex()
            .items_center()
            .justify_center()
            .children(picture.or(soft).map(|picture| {
                img(picture)
                    .absolute()
                    .top_0()
                    .left_0()
                    .size(px(36.))
                    .rounded(px(radius))
                    .object_fit(ObjectFit::Cover)
            }))
            .child(
                canvas(
                    move |at, _, _| {
                        arts.borrow_mut().insert(key, at);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            );
        if round && art.is_none() {
            face = face.child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight(650.))
                    .text_color(gpui::rgba(0xffffffeb))
                    .child(initial(&title)),
            );
        }
        // Play over a song's cover, while the pointer is on its row.
        if playing && let Hit::Song(found) = hit {
            let appearance = found.song.appearance_id;
            face = face.child(
                div()
                    .id(("search-play", index))
                    .absolute()
                    .size_full()
                    .rounded(px(radius))
                    .bg(gpui::rgba(0x00000073))
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |search, _, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            if search.asked == asked {
                                search.close(window, cx);
                                cx.emit(SearchEvent::Play(appearance));
                            }
                        }),
                    )
                    .child(icon("play", 14., gpui::rgb(0xffffff))),
            );
        }
        div()
            .id(("search-row", index))
            .h(px(50.))
            .px(px(10.))
            .rounded(px(9.))
            .flex()
            .items_center()
            .gap(px(12.))
            .when(lit, |row| row.bg(theme::fill()))
            .when(settled, |row| {
                row.on_mouse_move(cx.listener(move |search, _, _, cx| {
                    if search.asked != asked {
                        return;
                    }
                    let hovered = song.then_some(index);
                    if search.view.selected != index || search.hovered_song != hovered {
                        search.view.selected = index;
                        search.hovered_song = hovered;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |search, _, window, cx| {
                    if search.asked == asked {
                        search.pick(index, window, cx);
                    }
                }))
            })
            .child(face)
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().font_weight(FontWeight(550.)).child(title))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(theme::dim())
                            .child(sub),
                    ),
            )
            .into_any_element()
    }
}

fn empty(text: &'static str) -> impl IntoElement {
    div()
        .px(px(12.))
        .py(px(14.))
        .text_color(theme::dim())
        .child(text)
}

/// The dropdown card shadow, in each palette.
pub(crate) fn shadow() -> Vec<BoxShadow> {
    if theme::dark() {
        vec![
            theme::css_shadow(14., 40., gpui::rgba(0x00000073)),
            BoxShadow::new(px(0.), px(0.), Hsla::from(gpui::rgba(0xffffff12)))
                .spread_radius(px(1.)),
        ]
    } else {
        vec![
            theme::css_shadow(12., 34., gpui::rgba(0x1c223029)),
            BoxShadow::new(px(0.), px(0.), Hsla::from(gpui::rgba(0x1c223012)))
                .spread_radius(px(1.)),
        ]
    }
}

impl Render for SearchBox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.field.read(cx).focused(window);
        let width = f32::from(window.viewport_size().width);
        let wide = (width - 720.).clamp(180., 320.);
        let field = self.field.clone();
        let drop_bounds = Rc::clone(&self.drop_bounds);
        div()
            .id("search")
            .group("search")
            .w(px(wide))
            .flex_none()
            .h(px(32.))
            .px(px(12.))
            .rounded(px(9.))
            .bg(if focused {
                theme::raised()
            } else {
                theme::fill()
            })
            .when(!focused, |field| {
                field.hover(|style| style.bg(theme::fill2()))
            })
            .when(focused, |field| {
                field.shadow(vec![
                    BoxShadow::new(
                        px(0.),
                        px(0.),
                        Hsla::from(gpui::Rgba {
                            a: 0.55,
                            ..theme::acc()
                        }),
                    )
                    .spread_radius(px(2.)),
                ])
            })
            .flex()
            .items_center()
            .gap(px(8.))
            .text_color(theme::faint())
            // A press on the field's edge, not on its text, still gives it the keys.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|search, _, window, cx| {
                    cx.stop_propagation();
                    search.field.update(cx, |field, cx| {
                        let focus = gpui::Focusable::focus_handle(field, cx);
                        window.focus(&focus, cx);
                    });
                }),
            )
            // A press anywhere but the field and its results puts them
            // away, and gives the keys back: also with nothing typed, since a
            // control that keeps its press to itself gives the window
            // nothing to take them back with.
            .on_mouse_down_out(cx.listener(
                move |search, event: &gpui::MouseDownEvent, window, cx| {
                    let inside = drop_bounds
                        .get()
                        .is_some_and(|bounds| bounds.contains(&event.position));
                    if (search.view.open || search.focused(window, cx)) && !inside {
                        search.close(window, cx);
                    }
                },
            ))
            .child(icon("search", 16., theme::faint()))
            .child(field)
            .when(!focused, |bar| {
                // ⌘F, or Ctrl+F: the key that finds, as the system writes it.
                let key = window
                    .highest_precedence_binding_for_action(&crate::frame::Find)
                    .and_then(|binding| {
                        binding.keystrokes().first().map(crate::platform::key_text)
                    });
                bar.children(key.map(|key| div().text_size(px(12.)).child(key)))
            })
            .children(self.dropdown(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_hits_stay_visible_until_the_next_search_fails() {
        let mut view = View {
            text: "Harbour".to_owned(),
            hits: vec![Hit::Artist(ArtistRow {
                artist_id: Id::from_bytes([1; 16]),
                name: Some("Harbour".to_owned()),
                sort_name: None,
                album_count: 1,
                track_count: 1,
                art_id: None,
                portrait: false,
            })],
            settled: true,
            open: true,
            ..View::default()
        };
        view.start("North".to_owned());
        assert_eq!(view.hits.len(), 1);
        assert_eq!(view.selected, 0);
        assert!(!view.settled);
        assert!(view.dispatch());
        view.waiting = true;
        assert!(!view.dispatch());
        view.fail();
        assert!(view.failed && view.settled);
        assert!(!view.waiting);
        assert!(view.hits.get(view.selected).is_none());
    }

    /// The window around the field: it has the keys when nothing else
    /// does, as the shell does, and keeps the keys that reach it.
    struct Host {
        search: Entity<SearchBox>,
        home: gpui::FocusHandle,
        /// Something else that takes the keys when pressed.
        other: gpui::FocusHandle,
        /// How often the control that keeps its press to itself was pressed.
        kept: usize,
        keys: Vec<String>,
        chosen: usize,
        _chosen: Subscription,
    }

    impl Host {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            crate::input::bind_keys(cx);
            let home = cx.focus_handle();
            window.focus(&home, cx);
            let link = crate::link::unlinked(cx);
            let search = cx.new(|cx| SearchBox::new(link, home.clone(), window, cx));
            let chosen = cx.subscribe(&search, |host: &mut Self, _, _: &SearchEvent, _| {
                host.chosen += 1;
            });
            Self {
                search,
                home,
                other: cx.focus_handle(),
                kept: 0,
                keys: Vec::new(),
                chosen: 0,
                _chosen: chosen,
            }
        }
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .track_focus(&self.home)
                .size_full()
                .on_key_down(cx.listener(|host, event: &gpui::KeyDownEvent, _, _| {
                    host.keys.push(event.keystroke.key.clone());
                }))
                // At the right, as in the title bar: the dropdown hangs
                // from the field's right edge.
                .child(
                    div()
                        .h(px(40.))
                        .flex()
                        .justify_end()
                        .child(self.search.clone()),
                )
                .child(div().id("other").track_focus(&self.other).size(px(100.)))
                // As a tab in the title bar does.
                .child(div().id("keeps").size(px(100.)).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|host, _, _, cx| {
                        host.kept += 1;
                        cx.stop_propagation();
                    }),
                ))
        }
    }

    /// Below the field and the other controls: the page itself.
    const PAGE: gpui::Point<Pixels> = gpui::point(px(400.), px(400.));
    /// Where the control that takes the keys is.
    const OTHER: gpui::Point<Pixels> = gpui::point(px(50.), px(90.));
    /// Where the control that keeps its press to itself is.
    const KEEPS: gpui::Point<Pixels> = gpui::point(px(50.), px(190.));
    const HARBOUR: Id = Id::from_bytes([1; 16]);

    fn open(cx: &mut gpui::TestAppContext) -> (Entity<Host>, &mut gpui::VisualTestContext) {
        let (host, cx) = cx.add_window_view(Host::new);
        let search = host.read_with(cx, |host, _| host.search.clone());
        type_in(&search, "harbour", cx);
        (host, cx)
    }

    /// ⌘F, and `text` typed: the dropdown is down.
    fn type_in(search: &Entity<SearchBox>, text: &str, cx: &mut gpui::VisualTestContext) {
        search.update_in(cx, |search, window, cx| search.find(window, cx));
        cx.simulate_input(text);
        cx.run_until_parked();
        assert!(search.read_with(cx, |search, _| search.view.open));
    }

    /// The library's answer, drawn: one artist, Harbour.
    fn answer(search: &Entity<SearchBox>, cx: &mut gpui::VisualTestContext) {
        search.update(cx, |search, cx| {
            search.view.hits = vec![Hit::Artist(ArtistRow {
                artist_id: HARBOUR,
                name: Some("Harbour".to_owned()),
                sort_name: None,
                album_count: 1,
                track_count: 1,
                art_id: None,
                portrait: false,
            })];
            search.view.settled = true;
            cx.notify();
        });
        cx.run_until_parked();
    }

    /// Whether the page has the keys, and so the window's shortcuts work:
    /// keys pressed now reach it, and nothing is typed into the field.
    fn page_has_the_keys(host: &Entity<Host>, cx: &mut gpui::VisualTestContext) -> bool {
        let before = host.read_with(cx, |host, _| host.keys.len());
        let focused = cx.update(|window, cx| host.read(cx).home.is_focused(window));
        cx.simulate_keystrokes("space x");
        let (keys, typed) = host.read_with(cx, |host, cx| {
            (
                host.keys.get(before..).unwrap_or_default().to_vec(),
                host.search.read(cx).view.open,
            )
        });
        focused && keys == ["space", "x"] && !typed
    }

    #[gpui::test]
    fn choosing_a_result_with_return_gives_the_keys_back(cx: &mut gpui::TestAppContext) {
        let (host, cx) = open(cx);
        let search = host.read_with(cx, |host, _| host.search.clone());
        answer(&search, cx);
        cx.simulate_keystrokes("enter");
        assert_eq!(host.read_with(cx, |host, _| host.chosen), 1);
        assert!(page_has_the_keys(&host, cx));
    }

    #[gpui::test]
    fn choosing_a_result_with_a_click_gives_the_keys_back(cx: &mut gpui::TestAppContext) {
        let (host, cx) = open(cx);
        let search = host.read_with(cx, |host, _| host.search.clone());
        answer(&search, cx);
        let row = search.read_with(cx, |search, _| search.arts.borrow().get(&HARBOUR).copied());
        assert!(row.is_some(), "the row is drawn");
        let at = row.map(|row| row.center()).unwrap_or_default();
        cx.simulate_click(at, gpui::Modifiers::none());
        assert_eq!(host.read_with(cx, |host, _| host.chosen), 1);
        assert!(page_has_the_keys(&host, cx));
    }

    #[gpui::test]
    fn escape_gives_the_keys_back(cx: &mut gpui::TestAppContext) {
        let (host, cx) = open(cx);
        cx.simulate_keystrokes("escape");
        assert!(page_has_the_keys(&host, cx));
    }

    #[gpui::test]
    fn a_press_on_the_page_gives_the_keys_back(cx: &mut gpui::TestAppContext) {
        let (host, cx) = open(cx);
        cx.simulate_click(PAGE, gpui::Modifiers::none());
        assert!(page_has_the_keys(&host, cx));
    }

    #[gpui::test]
    fn a_press_on_a_control_that_keeps_it_gives_the_keys_back(cx: &mut gpui::TestAppContext) {
        let (host, cx) = cx.add_window_view(Host::new);
        let search = host.read_with(cx, |host, _| host.search.clone());
        // With nothing typed, and with the dropdown down.
        search.update_in(cx, |search, window, cx| search.find(window, cx));
        cx.simulate_click(KEEPS, gpui::Modifiers::none());
        assert!(page_has_the_keys(&host, cx));
        type_in(&search, "harbour", cx);
        cx.simulate_click(KEEPS, gpui::Modifiers::none());
        assert!(page_has_the_keys(&host, cx));
        assert_eq!(host.read_with(cx, |host, _| host.kept), 2);
    }

    #[gpui::test]
    fn a_press_on_another_control_leaves_it_the_keys(cx: &mut gpui::TestAppContext) {
        let (host, cx) = open(cx);
        cx.simulate_click(OTHER, gpui::Modifiers::none());
        let (open, other) = cx.update(|window, cx| {
            let host = host.read(cx);
            (
                host.search.read(cx).view.open,
                host.other.is_focused(window),
            )
        });
        assert!(!open && other);
    }

    #[gpui::test]
    fn search_opens_again_after_each_way_out(cx: &mut gpui::TestAppContext) {
        let (host, cx) = open(cx);
        let search = host.read_with(cx, |host, _| host.search.clone());
        for out in ["escape", "click", "return", "escape"] {
            match out {
                "escape" => cx.simulate_keystrokes("escape"),
                "click" => cx.simulate_click(PAGE, gpui::Modifiers::none()),
                _ => {
                    answer(&search, cx);
                    cx.simulate_keystrokes("enter");
                }
            }
            assert!(page_has_the_keys(&host, cx));
            // ⌘F again: the field has the keys, and typing goes into it.
            type_in(&search, "north", cx);
            let (typed, field) = cx.update(|window, cx| {
                let search = search.read(cx);
                (search.view.text.clone(), search.focused(window, cx))
            });
            assert_eq!((typed.as_str(), field), ("north", true));
        }
    }

    fn tiny_image() -> Arc<RenderImage> {
        Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(1, 1),
        )]))
    }

    #[test]
    fn search_pictures_stay_bounded_protect_active_hits_and_retry_failures() {
        let mut cache = Pictures::default();
        let active = Id::from_bytes([1; 16]);
        let evictable = Id::from_bytes([2; 16]);
        let failed = Id::from_bytes([250; 16]);
        let keep = HashSet::from([active]);

        // Failed requests clear in_flight so a later search can retry.
        assert!(cache.want(failed));
        assert!(!cache.want(failed));
        cache.finish_failed(failed);
        assert!(cache.want(failed));
        cache.finish_failed(failed);

        assert!(cache.want(active));
        assert!(cache.insert(active, tiny_image(), &keep).is_empty());
        assert!(cache.want(evictable));
        assert!(cache.insert(evictable, tiny_image(), &keep).is_empty());
        for i in 3..=(MAX_PICTURES + 5) {
            let id = Id::from_bytes([i as u8; 16]);
            assert!(cache.want(id));
            let evicted = cache.insert(id, tiny_image(), &keep);
            if i > MAX_PICTURES {
                assert_eq!(evicted.len(), 1);
            }
        }
        assert_eq!(cache.map.len(), MAX_PICTURES);
        // Active hit stays cached even after more than MAX_PICTURES inserts.
        assert!(cache.get(&active).is_some());
        assert!(cache.get(&evictable).is_none());
        // Evicted art can be requested again on a later search.
        assert!(cache.want(evictable));
    }
}
