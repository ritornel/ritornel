//! Search in the title bar (`lib/search.ts`, `frame/TitleBar.svelte`,
//! `frame/SearchResults.svelte`): what was typed, the results that drop down
//! under the field, which of them is chosen, and what choosing one does.
//! The server ranks each kind (`library.search`); this keeps the answers in
//! step with the typing and the keys.
//!
//! The results are artists, then records, then songs, a few of each. The
//! lit one starts at the kind the server says matched best, so that
//! "nocturne" lights the song Nocturne and not the album Smoke Nocturne
//! above it. Return before the answer has come asks at once and chooses its
//! first. A record's cover travels from the list to its page; a song opens
//! its album with the song marked, or plays when it is on no record.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext, Bounds, BoxShadow, Context, Entity, EventEmitter, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, ObjectFit, ParentElement, Pixels, Render,
    RenderImage, SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription, Task,
    Window, canvas, deferred, div, img, prelude::FluentBuilder, px,
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
/// How many of each kind drop down, as the prototype has it.
const ARTISTS: usize = 3;
const ALBUMS: usize = 4;
const SONGS: usize = 5;

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
}

pub(crate) struct SearchBox {
    link: Entity<Link>,
    pub field: Entity<Field>,
    view: View,
    /// Bumped by every keystroke: an answer for an older one is dropped.
    asked: u64,
    /// Return was pressed before the answer for what was typed came.
    waiting: bool,
    asking: Option<Task<()>>,
    /// When the dropdown came down, for it to rise into place.
    dropped: Option<Instant>,
    pictures: HashMap<Id, Arc<RenderImage>>,
    wanted: std::collections::HashSet<Id>,
    /// Where each row's picture is, for a record's cover to travel from.
    arts: Rc<std::cell::RefCell<HashMap<Id, Bounds<Pixels>>>>,
    /// Where the dropdown is, so a press in it does not close it.
    drop_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    hovered_song: Option<usize>,
    /// Closed without a window to hand: the field is emptied and let go
    /// when next drawn.
    close_field: bool,
    _field: Subscription,
}

impl EventEmitter<SearchEvent> for SearchBox {}

impl SearchBox {
    pub(crate) fn new(link: Entity<Link>, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| Field::new("Search", cx));
        let followed = cx.subscribe(&field, |search, _, event: &FieldEvent, cx| match event {
            FieldEvent::Changed(text) => search.typed(text.to_string(), cx),
            FieldEvent::Moved(by) => search.move_by(*by, cx),
            FieldEvent::Entered => search.enter(cx),
            FieldEvent::Escaped => search.close(cx),
        });
        Self {
            link,
            field,
            view: View {
                settled: true,
                ..View::default()
            },
            asked: 0,
            waiting: false,
            asking: None,
            dropped: None,
            pictures: HashMap::new(),
            wanted: std::collections::HashSet::new(),
            arts: Rc::default(),
            drop_bounds: Rc::default(),
            hovered_song: None,
            close_field: false,
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
    fn typed(&mut self, text: String, cx: &mut Context<Self>) {
        self.waiting = false;
        self.asked += 1;
        if text.trim().is_empty() {
            self.asking = None;
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
        self.view.text = text.clone();
        self.view.open = true;
        self.view.settled = false;
        let asked = self.asked;
        self.asking = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            this.update(cx, |search, cx| search.ask(text, asked, cx))
                .ok();
        }));
        cx.notify();
    }

    fn ask(&mut self, text: String, asked: u64, cx: &mut Context<Self>) {
        let Some(net) = self.link.read(cx).net() else {
            self.view.settled = true;
            self.view.failed = true;
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
        self.asking = Some(cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |search, cx| {
                if asked != search.asked {
                    return;
                }
                match answer {
                    Ok(Ok(results)) => {
                        let hits = hits_of(&results);
                        let selected = top_of(&hits, &results);
                        search.view = View {
                            text,
                            open: true,
                            hits,
                            selected,
                            settled: true,
                            failed: false,
                        };
                        search.pictures_wanted(cx);
                        if search.waiting {
                            search.waiting = false;
                            search.pick(selected, cx);
                        }
                    }
                    _ => {
                        search.view.settled = true;
                        search.view.failed = true;
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
            if !self.wanted.insert(art) {
                continue;
            }
            let px = 72;
            let http = Arc::clone(&net.art);
            let bucket = net.bucket(px);
            let read = net.rt().spawn(async move {
                let bytes = http.get(art, bucket).await?;
                tokio::task::spawn_blocking(move || crate::covers::decode_cover(&bytes, px))
                    .await
                    .ok()
                    .flatten()
            });
            cx.spawn(async move |this, cx| {
                if let Ok(Some((image, _, _))) = read.await {
                    this.update(cx, |search, cx| {
                        search.pictures.insert(art, Arc::new(image));
                        cx.notify();
                    })
                    .ok();
                }
            })
            .detach();
        }
    }

    /// The arrows: moves the choice, staying within the results.
    fn move_by(&mut self, by: i32, cx: &mut Context<Self>) {
        let Some(last) = self.view.hits.len().checked_sub(1) else {
            return;
        };
        let at = self.view.selected as i64 + i64::from(by);
        self.view.selected = at.clamp(0, last as i64) as usize;
        cx.notify();
    }

    /// Return: chooses the chosen result; typed faster than the settle, it
    /// asks at once and chooses the first of the answer.
    fn enter(&mut self, cx: &mut Context<Self>) {
        if !self.view.open {
            return;
        }
        if self.view.settled {
            self.pick(self.view.selected, cx);
            return;
        }
        self.waiting = true;
        let (text, asked) = (self.view.text.clone(), self.asked);
        self.ask(text, asked, cx);
    }

    /// Chooses the result at `index`, and the dropdown closes.
    fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(hit) = self.view.hits.get(index).cloned() else {
            return;
        };
        let event = match hit {
            Hit::Artist(row) => {
                SearchEvent::OpenArtist(row.artist_id, row.name.unwrap_or_default().into())
            }
            Hit::Album(tile) => {
                let id = tile.album.id();
                let image = tile.art_id.and_then(|art| self.pictures.get(&art).cloned());
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
        self.close(cx);
        cx.emit(event);
    }

    /// Esc, a press elsewhere, or a choice: the text goes, and the dropdown
    /// with it, and the field lets go of the keys.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.asked += 1;
        self.asking = None;
        self.waiting = false;
        self.view = View {
            settled: true,
            ..View::default()
        };
        self.dropped = None;
        self.close_field = true;
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
        if !self.view.open || !(self.view.settled || !self.view.hits.is_empty()) {
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
        let bounds = Rc::clone(&self.drop_bounds);
        card = card.child(
            canvas(move |at, _, _| bounds.set(Some(at)), |_, (), _, _| {})
                .absolute()
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
        let lit = index == self.view.selected;
        let picture = art.and_then(|art| self.pictures.get(&art).cloned());
        let scale = window.scale_factor();
        let soft = (art.is_none())
            .then(|| theme::soft_cached(&format!("{title}{sub}"), (36. * scale).ceil() as u32));
        let song = matches!(hit, Hit::Song(_));
        let playing = self.hovered_song == Some(index) && song;
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
                            search.close(cx);
                            cx.emit(SearchEvent::Play(appearance));
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
            .on_mouse_move(cx.listener(move |search, _, _, cx| {
                let hovered = song.then_some(index);
                if search.view.selected != index || search.hovered_song != hovered {
                    search.view.selected = index;
                    search.hovered_song = hovered;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |search, _, _, cx| search.pick(index, cx)))
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

/// The real card's `--shadow`, in each palette.
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
        if self.close_field {
            self.close_field = false;
            self.field.update(cx, |field, cx| field.clear(window, cx));
            window.blur(cx);
        }
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
            // A press anywhere but the field and its results puts them away.
            .on_mouse_down_out(
                cx.listener(move |search, event: &gpui::MouseDownEvent, _, cx| {
                    let inside = drop_bounds
                        .get()
                        .is_some_and(|bounds| bounds.contains(&event.position));
                    if search.view.open && !inside {
                        search.close(cx);
                    }
                }),
            )
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
