//! A list of songs (`parts/Tracks.svelte`): an album's tracks, in disc and
//! track order under a disc's heading when there is more than one, or an
//! artist's songs on no record, in as many columns as fit. A song by
//! someone other than the record's artist says whose it is.
//!
//! A double-click plays from that row, and the rest of the list follows.
//! The + plays the song next, and a row can be carried to Up next. The song playing is marked where its number
//! was. Stars show on a song that has some, and on any while the pointer is
//! on its row.

use std::collections::HashMap;

use gpui::{
    Context, Div, Entity, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    Window, div, prelude::FluentBuilder, px,
};
use ritornel_contract::{Id, PlayContext, PlaySource, QueuePlace, SongRow};

use crate::icons::icon;
use crate::player::{Player, stars_of};
use crate::theme;
use crate::words;

const HEADING: f32 = 30.;
const ROW: f32 = 44.;
/// Between the columns of a list in several.
const COLUMN_GAP: f32 = 56.;

pub(crate) struct Tracks {
    player: Entity<Player>,
    songs: Vec<SongRow>,
    /// The record's own credit: a song credited otherwise names its artist.
    credit: Option<String>,
    /// Why the songs are played from here: the album, or the artist.
    context: Option<PlayContext>,
    /// Whether the list has its header row.
    heading: bool,
    /// How wide the list is, and in how many columns.
    pub width: f32,
    pub columns: usize,
    /// The star the pointer is on, and the row it is in.
    star: Option<(usize, u32)>,
    /// The songs just queued with their +, for a moment.
    added: HashMap<Id, Task<()>>,
    /// The song search found, marked as a row chosen is, and where it was
    /// last drawn, for the page to bring it into view.
    pub mark: Option<Id>,
    pub marked: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// The song whose menu is open, or closing, and how strongly it shows.
    menued: Option<(String, f32)>,
    /// The record's cover, for a song carried to Up next to show: an
    /// album's songs have it, an artist's have none.
    pub art: Option<std::sync::Arc<gpui::RenderImage>>,
    _player: Subscription,
}

impl Tracks {
    pub(crate) fn new(player: Entity<Player>, heading: bool, cx: &mut Context<Self>) -> Self {
        // The song playing, and stars set anywhere, show here at once.
        let followed = cx.observe(&player, |_, _, cx| cx.notify());
        Self {
            player,
            songs: Vec::new(),
            credit: None,
            context: None,
            heading,
            width: 0.,
            columns: 1,
            star: None,
            added: HashMap::new(),
            mark: None,
            marked: std::rc::Rc::default(),
            menued: None,
            art: None,
            _player: followed,
        }
    }

    /// The songs listed, the credit they are shown against, and why they play.
    pub(crate) fn set(
        &mut self,
        songs: Vec<SongRow>,
        credit: Option<String>,
        context: PlayContext,
        cx: &mut Context<Self>,
    ) {
        self.songs = songs;
        self.credit = credit;
        self.context = Some(context);
        cx.notify();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.songs.is_empty()
    }

    /// Every song in the order shown: what a double-click plays.
    fn ids(&self) -> Vec<Id> {
        self.songs.iter().map(|song| song.appearance_id).collect()
    }

    /// Plays from the song at `at`, and the rest of the list follows.
    pub(crate) fn play_from(&mut self, at: usize, cx: &mut Context<Self>) {
        let ids = self.ids();
        let context = self.context;
        self.player.update(cx, |player, cx| {
            player
                .play(
                    PlaySource::Tracks { ids },
                    u32::try_from(at).ok(),
                    context,
                    false,
                    cx,
                )
                .detach();
        });
    }

    /// The + of a song: it plays next, and says so for a moment.
    pub(crate) fn play_next(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(song) = self.songs.get(at) else {
            return;
        };
        let id = song.appearance_id;
        let name = format!("“{}”", song.title.as_deref().unwrap_or("Untitled"));
        let context = self.context;
        let added = self.player.update(cx, |player, cx| {
            player.add(
                PlaySource::Tracks { ids: vec![id] },
                QueuePlace::Next,
                name,
                context,
                cx,
            )
        });
        let shown = cx.spawn(async move |this, cx| {
            let done = added.await;
            if done {
                this.update(cx, |_, cx| cx.notify()).ok();
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1100))
                    .await;
            }
            this.update(cx, |list, cx| {
                list.added.remove(&id);
                cx.notify();
            })
            .ok();
        });
        self.added.insert(id, shown);
    }

    /// A click on star `star` of the song at `at`.
    pub(crate) fn rate(&mut self, at: usize, star: u32, cx: &mut Context<Self>) {
        let Some((recording, said)) = self
            .songs
            .get(at)
            .map(|song| (song.recording_id, song.rating))
        else {
            return;
        };
        self.player.update(cx, |player, cx| {
            player.click_star(recording, said, star, cx)
        });
    }

    /// A song's menu (`songMenu`), at `at` in the window.
    pub(crate) fn menu(
        &mut self,
        index: usize,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(song) = self.songs.get(index) else {
            return;
        };
        let list = cx.entity().downgrade();
        let entries = crate::menu::song_menu(
            crate::menu::Song {
                appearance: song.appearance_id,
                title: song.title.clone(),
                release: song.release_id,
                album_title: song.album_title.clone(),
                credit: song.artist_credit.clone(),
                art: None,
                context: self.context,
            },
            Box::new(move |_, cx| {
                let _ = list.update(cx, |list, cx| list.play_from(index, cx));
            }),
        );
        let key = format!("song:{}", song.appearance_id);
        crate::menu::popup(key, entries, at, window, cx);
    }

    /// One song's row.
    fn row(
        &self,
        at: usize,
        song: &SongRow,
        playing: (Option<Id>, bool, f32),
        stars: u32,
        cx: &mut Context<Self>,
    ) -> (Div, bool) {
        let (playing_now, paused, eq_seconds) = playing;
        let mut animating = false;
        let by = song
            .artist_credit
            .as_ref()
            .map(|credit| credit.text.clone())
            .filter(|text| self.credit.as_ref().is_some_and(|credit| credit != text));
        let number = match (song.release_id, song.track) {
            (Some(_), Some(number)) => number.to_string(),
            _ => "·".to_owned(),
        };
        let current = playing_now == Some(song.appearance_id);
        let added = self.added.contains_key(&song.appearance_id);
        let group: SharedString = format!("track-{at}").into();
        // The song playing is marked where its number was; on the pointer,
        // Play takes the number's place, its target running from the row's
        // edge to the title, the whole height.
        let number_cell = if current {
            animating |= !paused;
            div()
                .w(px(26.))
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .child(crate::marks::eq(eq_seconds, 13., theme::ink()))
        } else {
            div()
                .relative()
                .w(px(26.))
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .child(
                    div()
                        .text_color(theme::faint())
                        .font_features(crate::bar::tabular())
                        .group_hover(group.clone(), |style| style.invisible())
                        .child(number),
                )
                .child(
                    div()
                        .id(("play", at))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(-9.))
                        .w(px(49.))
                        .pr(px(9.))
                        .flex()
                        .items_center()
                        .justify_end()
                        .invisible()
                        .group_hover(group.clone(), |style| style.visible())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |list, _, _, cx| {
                                cx.stop_propagation();
                                list.play_from(at, cx);
                            }),
                        )
                        .child(icon("play", 13., theme::ink())),
                )
        };
        let star_row = crate::marks::stars(
            format!("track-{at}").into(),
            stars,
            self.star
                .filter(|(row, _)| *row == at)
                .map(|(_, star)| star),
            true,
            13.,
            cx,
            move |list: &mut Self, star, over, _| {
                if over {
                    list.star = Some((at, star));
                } else if list.star == Some((at, star)) {
                    list.star = None;
                }
            },
            move |list: &mut Self, star, cx| list.rate(at, star, cx),
        );
        let stars_cell = div()
            .w(px(80.))
            .h_full()
            .flex()
            .items_center()
            .child(if stars > 0 {
                star_row
            } else {
                star_row
                    .invisible()
                    .group_hover(group.clone(), |style| style.visible())
            });
        // The + takes the place of the song's time while the pointer is on
        // the row, and says it worked for a moment.
        let plus = div()
            .id(("next", at))
            .absolute()
            .top(px(9.))
            .right(px(-4.))
            .size(px(26.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .when(!added, |plus| {
                plus.invisible()
                    .group_hover(group.clone(), |style| style.visible())
                    .hover(|style| style.bg(theme::fill2()))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |list, _, _, cx| list.play_next(at, cx)))
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
        let mut row = div()
            .id(("track", at))
            .px(px(14.))
            .flex()
            .items_center()
            .gap(px(18.))
            .group(group)
            .h(px(ROW))
            .flex_none()
            .text_size(px(15.))
            .border_b_1()
            .border_color(theme::mix(theme::line(), gpui::rgba(0xffffff00), 0.3))
            .hover(|style| {
                style
                    .bg(theme::fill())
                    .rounded(px(9.))
                    .border_color(gpui::rgba(0xffffff00))
            })
            // A double-click plays from this row, and the rest of the list follows.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |list, event: &MouseDownEvent, _, cx| {
                    if event.click_count == 2 {
                        list.play_from(at, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |list, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    list.menu(at, event.position, window, cx);
                }),
            );
        if current {
            row = row
                .bg(theme::fill())
                .rounded(px(9.))
                .border_color(gpui::rgba(0xffffff00));
        }
        // The row can be carried to Up next.
        row = row.on_drag(
            crate::carry::Carried::Songs {
                ids: vec![song.appearance_id],
                name: song.title.clone().unwrap_or_else(|| "Untitled".to_owned()),
                context: self.context,
                art: self.art.clone(),
            },
            crate::carry::lift,
        );
        let key = format!("song:{}", song.appearance_id);
        if let Some((_, shown)) = self.menued.as_ref().filter(|(menued, _)| *menued == key) {
            row = row
                .bg(crate::menu::fill(*shown))
                .rounded(px(9.))
                .border_color(gpui::rgba(0xffffff00));
        }
        if self.mark == Some(song.appearance_id) {
            let marked = std::rc::Rc::clone(&self.marked);
            row = row
                .relative()
                .bg(theme::fill2())
                .rounded(px(9.))
                .border_color(gpui::rgba(0xffffff00))
                .child(
                    gpui::canvas(
                        move |bounds, _, _| marked.set(Some(bounds)),
                        |_, (), _, _| {},
                    )
                    .absolute()
                    .size_full(),
                );
        }
        let row = row
            .child(number_cell)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::ink())
                    .when(current, |title| title.font_weight(FontWeight(650.)))
                    // A song credited otherwise runs on in the same line,
                    // its credit dim (`.by`), the whole cut short as one.
                    .child({
                        let title = song.title.clone().unwrap_or_else(|| "Untitled".to_owned());
                        match by {
                            None => gpui::StyledText::new(title),
                            Some(by) => {
                                let from = title.len();
                                let text = format!("{title} · {by}");
                                let to = text.len();
                                gpui::StyledText::new(text).with_highlights([(
                                    from..to,
                                    gpui::HighlightStyle {
                                        color: Some(theme::dim().into()),
                                        font_weight: Some(FontWeight::NORMAL),
                                        ..Default::default()
                                    },
                                )])
                            }
                        }
                    }),
            )
            .child(stars_cell)
            .child(div().w(px(52.)))
            .child(
                div()
                    .relative()
                    .w(px(44.))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .child(time)
                    .child(plus),
            );
        (div().child(row), animating)
    }
}

impl Render for Tracks {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let player = self.player.read(cx);
        let playing = (player.appearance(), !player.playing(), player.eq_seconds());
        let stars: Vec<u32> = self
            .songs
            .iter()
            .map(|song| stars_of(player.own(song.recording_id, song.rating), song.loved))
            .collect();
        let mut animating = false;
        self.menued = crate::menu::marked(window, cx).filter(|(key, _)| key.starts_with("song:"));
        let mut list = div().w(px(self.width)).flex().flex_col();
        if self.heading {
            list = list.child(
                div()
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(18.))
                    .h(px(HEADING))
                    .mb(px(2.))
                    .text_size(px(12.))
                    .text_color(theme::faint())
                    .border_b_1()
                    .border_color(theme::line())
                    .child(div().w(px(26.)).flex().justify_end().child("#"))
                    .child(div().flex_1().child("Title"))
                    .child(div().w(px(80.)).child("Rating"))
                    .child(div().w(px(52.)))
                    .child(div().w(px(44.)).flex().justify_end().child("Time")),
            );
        }
        let songs = self.songs.clone();
        if self.columns > 1 {
            // In several columns, running down each in turn.
            let rows = songs.len().div_ceil(self.columns);
            let mut columns = div().flex().items_start().gap(px(COLUMN_GAP));
            for column in 0..self.columns {
                let mut each = div().flex_1().min_w_0().flex().flex_col();
                for at in column * rows..((column + 1) * rows).min(songs.len()) {
                    if let Some(song) = songs.get(at) {
                        let (row, moving) =
                            self.row(at, song, playing, stars.get(at).copied().unwrap_or(0), cx);
                        animating |= moving;
                        each = each.child(row);
                    }
                }
                columns = columns.child(each);
            }
            list = list.child(columns);
        } else {
            let mut discs: Vec<i64> = Vec::new();
            for song in &songs {
                let disc = song.disc.unwrap_or(1);
                if !discs.contains(&disc) {
                    discs.push(disc);
                }
            }
            let several = discs.len() > 1;
            for (index, disc) in discs.iter().enumerate() {
                if several {
                    list = list.child(
                        div()
                            .mt(px(if index == 0 && self.heading { 18. } else { 30. }))
                            .mb(px(8.))
                            .pl(px(14.))
                            .child(crate::record::heading_sized(format!("Disc {disc}"), 15.)),
                    );
                }
                for (at, song) in songs
                    .iter()
                    .enumerate()
                    .filter(|(_, song)| song.disc.unwrap_or(1) == *disc)
                {
                    let (row, moving) =
                        self.row(at, song, playing, stars.get(at).copied().unwrap_or(0), cx);
                    animating |= moving;
                    list = list.child(row);
                }
            }
        }
        if animating {
            window.request_animation_frame();
        }
        list
    }
}
