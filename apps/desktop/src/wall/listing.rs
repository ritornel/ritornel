//! A view's rows as a list or as a table ("Library views" in
//! docs/internals/desktop.md).
//!
//! A list's row is a small picture, a name over a quieter line, and at its
//! right what the row has to say for itself. A table's row is one line of
//! columns under a bar that names them, which stays at the top of the page
//! while the rows pass under it; a column the view can be ordered by orders
//! it when its name is clicked.
//!
//! Every column's width is worked out from the row's (`columns`), the same
//! for the bar and for each row, so that they line up whatever is in them.
//!
//! A song plays on a double-click, the rest of the view following it, or
//! from the Play that takes its number's place under the pointer; its +
//! plays it next, its stars are set where they are shown, and its row can
//! be carried to Up next. A record or an artist opens on a click.

use std::sync::Arc;

use gpui::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Window, div, img, linear_color_stop, linear_gradient, prelude::FluentBuilder, px,
};
use ritornel_contract::{ContextKind, Id, PlayContext, PlaySource, QueuePlace, SongRow};

use super::{Detail, OpenAlbum, Subject, TileRow, Wall};
use crate::icons::icon;
use crate::player::stars_of;
use crate::theme;
use crate::views::{Entity, ViewSort};
use crate::words;

/// The height of a table's bar of columns.
pub(super) const COLUMN_BAR: f32 = 32.;
/// A row's margin at either end, and the space between a table's columns.
const PAD: f32 = 10.;
const GAP: f32 = 14.;

/// A row's picture: its soft colours, its cover when it has come and how
/// far it has faded in, and the size it is drawn at.
pub(super) struct Art {
    pub ground: (Hsla, Hsla),
    pub cover: Option<(Arc<gpui::RenderImage>, f32)>,
    pub side: f32,
    pub px: u32,
    /// Its cover is travelling: the place is left empty.
    pub away: bool,
}

/// What a column of a table shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shows {
    /// A song's number, or Play under the pointer, or the bars while it plays.
    Mark,
    /// A record's cover, or an artist's picture.
    Art,
    Title,
    Artist,
    Album,
    Year,
    Format,
    Plays,
    Rating,
    Time,
    Tracks,
    Albums,
    Songs,
}

/// One column of a table: what it shows, its heading, how wide it is, and
/// the order a click on its heading puts the view in.
#[derive(Debug, Clone, Copy)]
struct Column {
    shows: Shows,
    label: &'static str,
    width: f32,
    /// Set against its right edge, as numbers are.
    right: bool,
    sort: Option<ViewSort>,
}

/// The columns of a table of `entity` that is `width` wide, with their
/// widths: fixed ones as they are, and the rest sharing the room left by
/// weight. A narrow table leaves out what matters least.
fn columns(entity: Entity, width: f32, art: f32) -> Vec<Column> {
    // What it shows, its heading, its width or its share of the room left,
    // whether it is set right, its order, and the width it needs the table
    // to be.
    type Spec = (
        Shows,
        &'static str,
        Result<f32, f32>,
        bool,
        Option<ViewSort>,
        f32,
    );
    let specs: &[Spec] = match entity {
        Entity::Songs => &[
            (Shows::Mark, "#", Ok(26.), true, None, 0.),
            (
                Shows::Title,
                "Title",
                Err(2.4),
                false,
                Some(ViewSort::Title),
                0.,
            ),
            (
                Shows::Artist,
                "Artist",
                Err(1.5),
                false,
                Some(ViewSort::Album),
                0.,
            ),
            (Shows::Album, "Album", Err(1.7), false, None, 620.),
            (Shows::Year, "Year", Ok(44.), false, None, 900.),
            (Shows::Format, "Format", Ok(92.), false, None, 1000.),
            (
                Shows::Plays,
                "Plays",
                Ok(40.),
                true,
                Some(ViewSort::Plays),
                700.,
            ),
            (
                Shows::Rating,
                "Rating",
                Ok(76.),
                false,
                Some(ViewSort::Rating),
                0.,
            ),
            (Shows::Time, "Time", Ok(44.), true, None, 0.),
        ],
        Entity::Albums => &[
            (Shows::Art, "", Ok(0.), false, None, 0.),
            (
                Shows::Title,
                "Album",
                Err(2.2),
                false,
                Some(ViewSort::Title),
                0.,
            ),
            (
                Shows::Artist,
                "Artist",
                Err(1.6),
                false,
                Some(ViewSort::Artist),
                0.,
            ),
            (
                Shows::Year,
                "Year",
                Ok(44.),
                false,
                Some(ViewSort::Year),
                0.,
            ),
            (Shows::Tracks, "Tracks", Ok(48.), true, None, 700.),
            (Shows::Format, "Format", Ok(96.), false, None, 820.),
            (Shows::Time, "Length", Ok(64.), true, None, 0.),
        ],
        Entity::Artists => &[
            (Shows::Art, "", Ok(0.), false, None, 0.),
            (
                Shows::Title,
                "Artist",
                Err(1.),
                false,
                Some(ViewSort::Name),
                0.,
            ),
            (Shows::Albums, "Albums", Ok(72.), true, None, 0.),
            (Shows::Songs, "Songs", Ok(72.), true, None, 0.),
        ],
    };
    let kept: Vec<&Spec> = specs.iter().filter(|spec| width >= spec.5).collect();
    let fixed: f32 = kept
        .iter()
        .map(|spec| match (spec.0, spec.2) {
            (Shows::Art, _) => art,
            (_, Ok(fixed)) => fixed,
            (_, Err(_)) => 0.,
        })
        .sum();
    let weights: f32 = kept.iter().filter_map(|spec| spec.2.err()).sum();
    let gaps = GAP * kept.len().saturating_sub(1) as f32;
    let room = (width - 2. * PAD - gaps - fixed).max(0.);
    kept.into_iter()
        .map(|&(shows, label, width, right, sort, _)| Column {
            shows,
            label,
            width: match (shows, width) {
                (Shows::Art, _) => art,
                (_, Ok(fixed)) => fixed,
                (_, Err(weight)) => (room * weight / weights.max(1.)).floor(),
            },
            right,
            sort,
        })
        .collect()
}

/// A row whose words have not been read: the same height, quiet bars.
pub(super) fn placeholder(index: usize, height: f32, table: bool) -> Div {
    let width = 120. + ((index * 53) % 160) as f32;
    let side = if table { 0. } else { height - 12. };
    div()
        .h(px(height))
        .px(px(PAD))
        .flex()
        .items_center()
        .gap(px(GAP))
        .when(side > 0., |row| {
            row.child(
                div()
                    .size(px(side))
                    .flex_none()
                    .rounded(px(6.))
                    .bg(theme::fill()),
            )
        })
        .child(
            div()
                .w(px(width))
                .h(px(9.))
                .rounded(px(5.))
                .bg(theme::fill()),
        )
}

/// A row's small picture: a record's cover with corners of `radius`, an
/// artist's round, and for what has none its soft colours, with an artist's
/// initial or a song's mark over them.
fn thumb(row: &TileRow, art: &Art, radius: f32) -> Div {
    let side = art.side;
    let radius = if row.round { side / 2. } else { radius };
    let mut picture = div()
        .relative()
        .size(px(side))
        .flex_none()
        .rounded(px(radius));
    if art.away {
        return picture;
    }
    picture = picture.bg(linear_gradient(
        180.,
        linear_color_stop(art.ground.0, 0.),
        linear_color_stop(art.ground.1, 1.),
    ));
    let song = row.song().is_some();
    let cover = art.cover.clone().or_else(|| {
        row.no_cover
            .as_ref()
            .filter(|_| !song)
            .map(|name| (theme::soft_cached(name, art.px), 1.))
    });
    if let Some((image, opacity)) = cover {
        picture = picture.child(
            img(image)
                .absolute()
                .top_0()
                .left_0()
                .size(px(side))
                .rounded(px(radius))
                .opacity(opacity),
        );
    }
    if row.round && row.art.is_none() {
        picture = picture.child(
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
    if song {
        picture = picture.child(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(icon("note", (side * 0.42).max(11.), gpui::rgba(0xffffffb8))),
        );
    }
    // A hairline inside the edge, so a dark cover meets a dark page cleanly.
    picture.child(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size(px(side))
            .rounded(px(radius))
            .border_1()
            .border_color(gpui::rgba(0x80808024)),
    )
}

/// A format in a quiet outline: "FLAC 24/96".
fn pill(format: SharedString) -> Div {
    div()
        .flex_none()
        .h(px(18.))
        .px(px(6.))
        .rounded(px(5.))
        .border_1()
        .border_color(theme::line())
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(14.))
        .text_color(theme::dim())
        .font_features(crate::bar::tabular())
        .whitespace_nowrap()
        .child(format)
}

/// Words in a cell, cut short where they do not fit.
fn words_in(text: impl Into<SharedString>, color: gpui::Rgba) -> Div {
    div()
        .w_full()
        .truncate()
        .text_color(color)
        .child(text.into())
}

impl Wall {
    /// Why a song is played from here: from a list of them.
    const CONTEXT: Option<PlayContext> = Some(PlayContext {
        kind: ContextKind::Query,
        reference: None,
    });

    /// Plays the view of songs from the row at `index`, the rest following
    /// in the view's order: the server reads the songs out of the view, so
    /// a list of any length plays on.
    pub(crate) fn play_from(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(view) = self.view_id().map(str::to_owned) else {
            return;
        };
        self.player.update(cx, |player, cx| {
            player
                .play(
                    PlaySource::View { view },
                    u32::try_from(index).ok(),
                    Self::CONTEXT,
                    false,
                    cx,
                )
                .detach();
        });
    }

    /// The + of a song: it plays next, and says so for a moment.
    fn play_next(&mut self, song: &SongRow, cx: &mut Context<Self>) {
        let id = song.appearance_id;
        let name = format!("“{}”", song.title.as_deref().unwrap_or("Untitled"));
        let added = self.player.update(cx, |player, cx| {
            player.add(
                PlaySource::Tracks { ids: vec![id] },
                QueuePlace::Next,
                name,
                Self::CONTEXT,
                cx,
            )
        });
        let shown = cx.spawn(async move |wall, cx| {
            if added.await {
                wall.update(cx, |_, cx| cx.notify()).ok();
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1100))
                    .await;
            }
            wall.update(cx, |wall, cx| {
                wall.added.remove(&id);
                cx.notify();
            })
            .ok();
        });
        self.added.insert(id, shown);
    }

    /// A song's menu, at `at` in the window.
    pub(super) fn song_menu(
        &mut self,
        index: usize,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.row(index) else { return };
        let Some(song) = row.song() else { return };
        let wall = cx.entity().downgrade();
        let entries = crate::menu::song_menu(
            crate::menu::Song {
                appearance: song.appearance_id,
                title: song.title.clone(),
                release: song.release_id,
                album_title: song.album_title.clone(),
                credit: song.artist_credit.clone(),
                art: None,
                context: Self::CONTEXT,
            },
            Box::new(move |_, cx| {
                let _ = wall.update(cx, |wall, cx| wall.play_from(index, cx));
            }),
        );
        crate::menu::popup(song_key(song.appearance_id), entries, at, window, cx);
    }

    /// What every row answers the pointer with, whatever it is drawn as:
    /// a song plays on a double-click, has its menu, and can be carried to
    /// Up next; a record or an artist opens on a click and has its menu.
    fn answering(
        &self,
        element: Stateful<Div>,
        index: usize,
        row: &Arc<TileRow>,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let element = element.when(selected, |row| {
            row.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        });
        if let Some(song) = row.song() {
            return element
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |wall, event: &MouseDownEvent, _, cx| {
                        if event.click_count == 2 {
                            wall.play_from(index, cx);
                        }
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |wall, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        wall.song_menu(index, event.position, window, cx);
                    }),
                )
                .on_drag(
                    crate::carry::Carried::Songs {
                        ids: vec![song.appearance_id],
                        name: row.title.to_string(),
                        context: Self::CONTEXT,
                        art: None,
                    },
                    crate::carry::lift,
                );
        }
        let (open_row, menu_row) = (Arc::clone(row), Arc::clone(row));
        let element = element
            .cursor_pointer()
            .on_click(cx.listener(move |wall, event: &gpui::ClickEvent, _, cx| {
                if event.standard_click()
                    && let Some(art) = wall.art_bounds(index)
                {
                    cx.emit(OpenAlbum {
                        row: Arc::clone(&open_row),
                        index,
                        art,
                    });
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |wall, event: &MouseDownEvent, window, cx| {
                    wall.menu_open = Some(index);
                    cx.notify();
                    crate::menu::record_menu(Arc::clone(&menu_row), event.position, window, cx);
                }),
            );
        match row.subject {
            Subject::Album(album) => element.on_drag(
                crate::carry::Carried::Record {
                    album,
                    name: row.title.to_string(),
                    art: None,
                },
                crate::carry::lift,
            ),
            Subject::Artist(_) | Subject::Song => element,
        }
    }

    /// How a row stands out from the others: the song playing, the row
    /// selected or whose menu is open, and the artist come back from.
    fn standing(&self, row: &TileRow, index: usize, here: f32, cx: &Context<Self>) -> Standing {
        let current = row
            .song()
            .is_some_and(|song| self.player.read(cx).appearance() == Some(song.appearance_id));
        let selected = self.menu_open == Some(index)
            || self
                .selected
                .as_ref()
                .is_some_and(|selected| selected.id == row.id);
        let menued = row.song().map_or(0., |song| {
            crate::menu::strength(self.menued.as_ref(), &song_key(song.appearance_id))
        });
        Standing {
            current,
            selected,
            menued,
            here,
        }
    }

    /// A song's stars: shown when it has some, and on any under the
    /// pointer, where a click sets them.
    fn stars_cell(
        &self,
        index: usize,
        song: &SongRow,
        group: &SharedString,
        cx: &mut Context<Self>,
    ) -> Div {
        let shown = stars_of(
            self.player
                .read(cx)
                .own(song.recording_id, song.rating, Some(self.revision)),
            song.loved,
        );
        let (recording, said) = (song.recording_id, song.rating);
        let stars = crate::marks::stars(
            format!("row-{index}").into(),
            shown,
            self.star
                .filter(|(row, _)| *row == index)
                .map(|(_, star)| star),
            true,
            13.,
            cx,
            move |wall: &mut Self, star, over, _| {
                if over {
                    wall.star = Some((index, star));
                } else if wall.star == Some((index, star)) {
                    wall.star = None;
                }
            },
            move |wall: &mut Self, star, cx| {
                let revision = wall.revision;
                wall.player.update(cx, |player, cx| {
                    player.click_star(recording, said, Some(revision), star, cx);
                });
            },
        );
        if shown > 0 {
            stars
        } else {
            stars
                .invisible()
                .group_hover(group.clone(), |style| style.visible())
        }
    }

    /// A song's length, which its + takes the place of under the pointer.
    fn time_cell(
        &self,
        index: usize,
        song: &SongRow,
        group: &SharedString,
        cx: &mut Context<Self>,
    ) -> Div {
        let added = self.added.contains_key(&song.appearance_id);
        let queued = song.clone();
        let plus = div()
            .id(("next", index))
            .absolute()
            .right(px(-5.))
            .size(px(24.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .when(!added, |plus| {
                plus.invisible()
                    .group_hover(group.clone(), |style| style.visible())
                    .hover(theme::hovered_on_fill)
            })
            .active(theme::pressed)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |wall, _, _, cx| wall.play_next(&queued, cx)))
            .child(if added {
                icon("check", 11., theme::acc())
            } else {
                icon("plus", 11., theme::dim())
            });
        let time = div()
            .text_color(theme::dim())
            .font_features(crate::bar::tabular())
            .when(added, |time| time.invisible())
            .group_hover(group.clone(), |style| style.invisible())
            .child(
                song.duration_ms
                    .map(|ms| words::clock(ms.max(0) as u64))
                    .unwrap_or_default(),
            );
        div()
            .relative()
            .h_full()
            .flex()
            .items_center()
            .justify_end()
            .child(time)
            .child(plus)
    }

    /// A song's place in a table: its number on its record, Play under
    /// the pointer, or the moving bars while it plays.
    fn mark_cell(
        &self,
        index: usize,
        song: &SongRow,
        current: bool,
        group: &SharedString,
        cx: &mut Context<Self>,
    ) -> Div {
        let cell = div()
            .relative()
            .h_full()
            .flex()
            .items_center()
            .justify_end();
        if current {
            return cell.child(self.player.read(cx).equalizer(cx).draw(theme::acc()));
        }
        let number = match (song.release_id, song.track) {
            (Some(_), Some(number)) => number.to_string(),
            _ => "·".to_owned(),
        };
        cell.child(
            div()
                .text_color(theme::faint())
                .font_features(crate::bar::tabular())
                .group_hover(group.clone(), |style| style.invisible())
                .child(number),
        )
        .child(
            div()
                .id(("play", index))
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(-PAD))
                .right(px(-GAP / 2.))
                .pr(px(GAP / 2.))
                .flex()
                .items_center()
                .justify_end()
                .invisible()
                .group_hover(group.clone(), |style| style.visible())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |wall, _, _, cx| {
                        cx.stop_propagation();
                        wall.play_from(index, cx);
                    }),
                )
                .child(icon("play", 12., theme::ink())),
        )
    }

    /// One row of a table.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn table_row(
        &self,
        index: usize,
        row: &Arc<TileRow>,
        art: Art,
        here: f32,
        width: f32,
        height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = self.config().entity;
        let standing = self.standing(row, index, here, cx);
        let group: SharedString = format!("row-{index}").into();
        let mut line = div()
            .id(("row", index))
            .group(group.clone())
            .h(px(height))
            .px(px(PAD))
            .flex()
            .items_center()
            .gap(px(GAP))
            .text_size(px(13.))
            .line_height(px(18.))
            .border_b_1()
            .border_color(theme::mix(theme::line(), gpui::rgba(0xffffff00), 0.45))
            .hover(|style| {
                theme::hovered(style)
                    .rounded(px(7.))
                    .border_color(gpui::rgba(0xffffff00))
            });
        line = standing.worn(line, 7.);
        line = self.answering(line, index, row, standing.selected, cx);
        for column in columns(entity, width, art.side) {
            let cell = div()
                .w(px(column.width))
                .flex_none()
                .h_full()
                .flex()
                .items_center()
                .when(column.right, |cell| {
                    cell.justify_end().font_features(crate::bar::tabular())
                });
            let dim = theme::dim();
            let cell = match (&row.detail, column.shows) {
                (_, Shows::Art) => cell.child(thumb(row, &art, 4.)),
                (_, Shows::Title) => cell.child(
                    words_in(row.title.clone(), theme::ink()).when(standing.current, |title| {
                        title.font_weight(FontWeight(650.))
                    }),
                ),
                (Detail::Song { song, .. }, Shows::Mark) => cell.child(
                    self.mark_cell(index, song, standing.current, &group, cx)
                        .w_full(),
                ),
                (Detail::Song { .. } | Detail::Album { .. }, Shows::Artist) => {
                    cell.child(words_in(row.artist.clone(), dim))
                }
                (Detail::Song { song, .. }, Shows::Album) => {
                    cell.child(words_in(song.album_title.clone().unwrap_or_default(), dim))
                }
                (Detail::Song { song, .. }, Shows::Year) => cell
                    .font_features(crate::bar::tabular())
                    .child(words_in(year_words(song.year), dim)),
                (Detail::Album { year, .. }, Shows::Year) => cell
                    .font_features(crate::bar::tabular())
                    .child(words_in(year_words(*year), dim)),
                (Detail::Song { format, .. } | Detail::Album { format, .. }, Shows::Format) => cell
                    .text_size(px(12.))
                    .font_features(crate::bar::tabular())
                    .child(words_in(format.clone().unwrap_or_default(), theme::faint())),
                (Detail::Song { song, .. }, Shows::Plays) => {
                    cell.text_color(dim).child(if song.play_count > 0 {
                        words::thousands(song.play_count)
                    } else {
                        String::new()
                    })
                }
                (Detail::Song { song, .. }, Shows::Rating) => {
                    cell.child(self.stars_cell(index, song, &group, cx))
                }
                (Detail::Song { song, .. }, Shows::Time) => {
                    cell.child(self.time_cell(index, song, &group, cx).w_full())
                }
                (Detail::Album { duration_ms, .. }, Shows::Time) => {
                    cell.text_color(dim).child(words::length(*duration_ms))
                }
                (Detail::Album { tracks, .. }, Shows::Tracks) => {
                    cell.text_color(dim).child(words::thousands(*tracks))
                }
                (Detail::Artist { albums, .. }, Shows::Albums) => {
                    cell.text_color(dim).child(words::thousands(*albums))
                }
                (Detail::Artist { songs, .. }, Shows::Songs) => {
                    cell.text_color(dim).child(words::thousands(*songs))
                }
                _ => cell,
            };
            line = line.child(cell);
        }
        line.into_any_element()
    }

    /// A table's bar of columns, `y` from the top of the page: over the
    /// first row at rest, and held at the top once the rows pass under it.
    pub(super) fn column_bar(
        &self,
        left: f32,
        y: f32,
        width: f32,
        art: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = self.config();
        let (entity, sort) = (view.entity, view.sort);
        let mut bar = div()
            .absolute()
            .left(px(left))
            .top(px(y))
            .w(px(width))
            .h(px(COLUMN_BAR))
            .px(px(PAD))
            .flex()
            .items_center()
            .gap(px(GAP))
            .bg(theme::bg())
            .border_b_1()
            .border_color(theme::line())
            .text_size(px(12.))
            .line_height(px(16.))
            .font_weight(FontWeight(500.))
            .text_color(theme::faint())
            // The rows under it take no pointer through it.
            .occlude();
        for column in columns(entity, width, art) {
            let lit = column.sort == Some(sort);
            let mut cell = div()
                .id(SharedString::from(format!("column-{}", column.label)))
                .w(px(column.width))
                .flex_none()
                .h_full()
                .flex()
                .items_center()
                .gap(px(3.))
                .when(column.right, |cell| cell.justify_end())
                .when(lit, |cell| cell.text_color(theme::ink()))
                .child(column.label);
            if lit {
                cell = cell.child(icon("chevron", 11., theme::acc()));
            }
            if let Some(order) = column.sort.filter(|_| !lit) {
                cell = cell
                    .cursor_pointer()
                    .hover(theme::hovered_ink)
                    .active(theme::pressed)
                    .on_click(cx.listener(move |wall, _, _, cx| {
                        wall.edit(|view| view.sort = order, cx);
                    }));
            }
            bar = bar.child(cell);
        }
        bar.into_any_element()
    }

    /// One row of a list.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn list_row(
        &self,
        index: usize,
        row: &Arc<TileRow>,
        art: Art,
        here: f32,
        width: f32,
        height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let standing = self.standing(row, index, here, cx);
        let group: SharedString = format!("row-{index}").into();
        let quiet = |text: String| {
            div()
                .flex_none()
                .text_size(px(13.))
                .text_color(theme::dim())
                .font_features(crate::bar::tabular())
                .whitespace_nowrap()
                .child(text)
        };
        // The line under the name, and what the row says at its right.
        let (under, mut facts): (SharedString, Vec<Div>) = match &row.detail {
            Detail::Artist { albums, songs } => (
                format!(
                    "{} · {}",
                    words::plural(*albums, "album"),
                    words::plural(*songs, "song")
                )
                .into(),
                Vec::new(),
            ),
            Detail::Album {
                year,
                tracks,
                duration_ms,
                format,
            } => {
                let mut facts = Vec::new();
                if width >= 760. {
                    facts.extend(format.clone().map(pill));
                }
                if width >= 620. {
                    facts.push(
                        quiet(words::plural(*tracks, "track"))
                            .w(px(72.))
                            .flex()
                            .justify_end(),
                    );
                }
                facts.push(
                    quiet(words::length(*duration_ms))
                        .w(px(78.))
                        .flex()
                        .justify_end(),
                );
                facts.push(quiet(year_words(*year)).w(px(40.)).flex().justify_end());
                (row.artist.clone(), facts)
            }
            Detail::Song { song, format } => {
                let under = match &song.album_title {
                    Some(album) => format!("{} — {album}", row.artist),
                    None => row.artist.to_string(),
                };
                let mut facts = Vec::new();
                if width >= 820. {
                    facts.extend(format.clone().map(pill));
                }
                if width >= 620. {
                    let plays = if song.play_count > 0 {
                        words::plural(song.play_count, "play")
                    } else {
                        String::new()
                    };
                    facts.push(quiet(plays).w(px(72.)).flex().justify_end());
                }
                facts.push(
                    div()
                        .w(px(76.))
                        .flex_none()
                        .child(self.stars_cell(index, song, &group, cx)),
                );
                facts.push(
                    div()
                        .w(px(44.))
                        .h_full()
                        .flex_none()
                        .text_size(px(13.))
                        .child(self.time_cell(index, song, &group, cx).w_full()),
                );
                (under.into(), facts)
            }
        };
        // A song's picture is its Play under the pointer, and the bars
        // while it plays.
        let mut picture = thumb(row, &art, 6.);
        if row.song().is_some() && !art.away {
            let over = div()
                .absolute()
                .size_full()
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x00000066));
            picture = if standing.current {
                picture
                    .child(over.child(self.player.read(cx).equalizer(cx).draw(gpui::rgb(0xffffff))))
            } else {
                picture.child(
                    over.id(("play", index))
                        .invisible()
                        .group_hover(group.clone(), |style| style.visible())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |wall, _, _, cx| {
                                cx.stop_propagation();
                                wall.play_from(index, cx);
                            }),
                        )
                        .child(icon(
                            "play",
                            (art.side * 0.34).max(11.),
                            gpui::rgb(0xffffff),
                        )),
                )
            };
        }
        let mut line = div()
            .id(("row", index))
            .group(group.clone())
            .h(px(height))
            .px(px(PAD))
            .flex()
            .items_center()
            .gap(px(GAP))
            .border_b_1()
            .border_color(theme::mix(theme::line(), gpui::rgba(0xffffff00), 0.45))
            .hover(|style| {
                theme::hovered(style)
                    .rounded(px(9.))
                    .border_color(gpui::rgba(0xffffff00))
            });
        line = standing.worn(line, 9.);
        line = self.answering(line, index, row, standing.selected, cx);
        // A compact row has room for one line: the name, and what is under
        // it beside it.
        let one_line = height < 54.;
        let name = div()
            .text_size(px(14.5))
            .line_height(px(20.))
            .font_weight(if standing.current {
                FontWeight(650.)
            } else {
                FontWeight::SEMIBOLD
            })
            .text_color(theme::ink())
            .truncate()
            .child(row.title.clone());
        let under = div()
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(theme::dim())
            .truncate()
            .child(under);
        let words = if one_line {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(12.))
                .child(name.flex_none().max_w(gpui::relative(0.6)))
                .child(under.flex_1().min_w_0())
        } else {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(name)
                .child(under)
        };
        facts.reverse();
        let mut right = div()
            .flex_none()
            .h_full()
            .flex()
            .flex_row_reverse()
            .items_center()
            .gap(px(GAP));
        for fact in facts {
            right = right.child(fact);
        }
        line.child(picture)
            .child(words)
            .child(right)
            .into_any_element()
    }
}

/// How a row stands out from the others.
struct Standing {
    /// It is the song playing.
    current: bool,
    selected: bool,
    /// How strongly its menu shows, 0 with none.
    menued: f32,
    /// How strongly it is marked as where one came back from.
    here: f32,
}

impl Standing {
    /// The row wearing it: a ground that takes the place of its hairline.
    fn worn(&self, row: Stateful<Div>, radius: f32) -> Stateful<Div> {
        let ground = if self.here > 0. {
            Some(gpui::Rgba {
                a: 0.22 * self.here,
                ..theme::acc()
            })
        } else if self.menued > 0. {
            Some(crate::menu::fill(self.menued))
        } else if self.selected {
            Some(theme::fill2())
        } else if self.current {
            Some(theme::fill())
        } else {
            None
        };
        match ground {
            Some(ground) => row
                .bg(ground)
                .rounded(px(radius))
                .border_color(gpui::rgba(0xffffff00)),
            None => row,
        }
    }
}

/// What a song's menu marks its row by.
fn song_key(appearance: Id) -> String {
    format!("song:{appearance}")
}

fn year_words(year: Option<i64>) -> String {
    year.map(|year| year.to_string()).unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn a_tables_columns_fill_its_width_and_a_narrow_one_leaves_some_out() {
        for entity in Entity::ALL {
            for width in [560., 760., 1020., 1920.] {
                let columns = columns(entity, width, 26.);
                let used: f32 = columns.iter().map(|column| column.width).sum::<f32>()
                    + GAP * (columns.len() - 1) as f32
                    + 2. * PAD;
                // Never wider than the row, and no more than a point a
                // column short of it.
                assert!(used <= width, "{entity:?} at {width}: {used}");
                assert!(
                    width - used < columns.len() as f32,
                    "{entity:?} at {width}: {used}"
                );
                assert!(
                    columns.iter().all(|column| column.width > 0.),
                    "{columns:?}"
                );
            }
        }
        let shown = |width: f32| -> Vec<Shows> {
            columns(Entity::Songs, width, 0.)
                .iter()
                .map(|column| column.shows)
                .collect()
        };
        assert!(shown(1020.).contains(&Shows::Format));
        assert!(!shown(960.).contains(&Shows::Format));
        assert!(shown(960.).contains(&Shows::Year));
        assert_eq!(
            shown(560.),
            [
                Shows::Mark,
                Shows::Title,
                Shows::Artist,
                Shows::Rating,
                Shows::Time
            ]
        );
    }

    #[test]
    fn a_column_orders_the_view_only_by_what_its_entity_can_be_ordered_by() {
        for entity in Entity::ALL {
            for column in columns(entity, 1400., 26.) {
                if let Some(sort) = column.sort {
                    assert!(entity.sorts().contains(&sort), "{entity:?} {column:?}");
                }
            }
        }
    }
}
