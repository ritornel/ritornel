//! Up next: a card, not a column, with space all round it, under the search
//! field and exactly as wide ("Queue, context menus, and Settings" in
//! docs/internals/desktop.md). With room it moves the page over; in a
//! narrow window it lies over the page. The shell places and flies it.
//!
//! The song on heads it, set apart and fixed in place. Under it, what plays
//! after, in the order it plays: one plain list. A double-click plays a
//! song now, a drag puts it elsewhere, its × takes it out, and a right-click
//! opens its menu. Songs and records dropped on the card go where they land:
//! at its top they play next, between two songs after the one above, below
//! them at the end.
//!
//! GPUI has no native backdrop blur, so the card uses a translucent fill
//! over the simulated frost layer (`frost.rs`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, BoxShadow, Context, DragMoveEvent, Entity, FontWeight, Hsla, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ObjectFit, ParentElement, Pixels, Render,
    RenderImage, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, StyledImage,
    Subscription, Task, Window, canvas, div, img, prelude::FluentBuilder, px,
};
use ritornel_contract::{Id, QueueEntry, QueuePlace, Repeat, TrackSummary};

use crate::carry::{self, Carried};
use crate::icons::icon;
use crate::link::{Link, LinkEvent};
use crate::net::NoArt;
use crate::player::{Player, stars_of};
use crate::theme;
use crate::words::{clock, length, plural};

/// The card's width, and where it stands in the window: clear of the title
/// bar, the window's edge and the player bar.
pub(crate) const WIDTH: f32 = 320.;
pub(crate) const TOP: f32 = 60.;
pub(crate) const RIGHT: f32 = 16.;
pub(crate) const BOTTOM: f32 = 92.;
/// The room the card takes when it moves the page over: its 320 points and
/// the space beside it.
pub(crate) const RAIL: f32 = 328.;
/// The narrowest window the card moves the page over in, rather than lying
/// over it.
pub(crate) const ROOM_FOR_CARD: f32 = 1180.;
/// Its flight in and out: 24 points from the right, fading, in 260 ms and
/// out in 160 ms, eased by `cubicOut`.
pub(crate) const FLY_X: f32 = 24.;
pub(crate) const FLY_IN_MS: f32 = 260.;
pub(crate) const FLY_OUT_MS: f32 = 160.;
/// The card's corners.
const RADIUS: f32 = 16.;
/// How long a song just added shows in the accent (`arrived`).
const ARRIVED_MS: f32 = 1400.;
/// Covers kept beyond what the queue currently shows before pruning.
const MAX_PICTURES: usize = 64;
/// Cadence for the 13 px equalizer bars while playing (~30 Hz).
const EQ_TICK: Duration = Duration::from_millis(33);

/// The player state `UpNextCard` displays, so 250 ms progress ticks that do
/// not change any visible field do not trigger full card rebuilds or art checks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CardSnapshot {
    entry_id: Option<Id>,
    art_id: Option<Id>,
    playing: bool,
    repeat: Repeat,
    shuffle: bool,
    queue_revision: Option<u64>,
    up_next_version: u64,
    ratings_revision: Option<u64>,
    ratings_version: u64,
    entries_len: usize,
    total: u64,
    left_minutes: u64,
}

impl CardSnapshot {
    fn of(player: &Player) -> Self {
        let state = player.state.as_ref();
        let entry_id = state.and_then(|s| s.entry_id);
        let art_id = state
            .filter(|s| s.entry_id.is_some())
            .and_then(|s| s.track.as_ref())
            .and_then(|t| t.art_id);
        let up_next = player.up_next.as_ref();
        let ahead: u64 = up_next.map_or(0, |q| {
            q.entries
                .iter()
                .map(|entry| entry.track.duration_ms.unwrap_or(0))
                .sum()
        });
        let here = state
            .and_then(|s| s.duration_ms)
            .unwrap_or(0)
            .saturating_sub(player.position());
        let left_minutes = (ahead + here + 30_000) / 60_000;
        Self {
            entry_id,
            art_id,
            playing: player.playing(),
            repeat: state.map_or(Repeat::Off, |s| s.repeat),
            shuffle: state.is_some_and(|s| s.shuffle),
            queue_revision: up_next.map(|q| q.revision),
            up_next_version: player.up_next_version(),
            ratings_revision: up_next.map(|q| q.ratings_revision),
            ratings_version: player.ratings_version(),
            entries_len: up_next.map_or(0, |q| q.entries.len()),
            total: up_next.map_or(0, |q| q.total),
            left_minutes,
        }
    }
}

/// The room the page gives Up next's card, as the shell last set it: the
/// rail when the card moves the page over, or nothing.
pub(crate) struct Rail(pub f32);

impl gpui::Global for Rail {}

/// How much narrower the page is laid out for Up next's card.
pub(crate) fn rail(cx: &gpui::App) -> f32 {
    cx.try_global::<Rail>().map_or(0., |rail| rail.0)
}

/// Cubic ease-out curve.
pub(crate) fn cubic_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

/// Where something carried would land: before or after a row, or in the
/// card's open space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Row { index: usize, after: bool },
    End,
}

/// Where a drop at `at` puts what is carried, among `entries`.
fn place_of(at: Target, entries: &[QueueEntry]) -> QueuePlace {
    let above = match at {
        Target::End if entries.is_empty() => return QueuePlace::Next,
        Target::End => return QueuePlace::End,
        Target::Row { index, after: true } => entries.get(index),
        Target::Row {
            index,
            after: false,
        } => index.checked_sub(1).and_then(|above| entries.get(above)),
    };
    above.map_or(QueuePlace::Next, |above| QueuePlace::After {
        entry_id: above.entry_id,
    })
}

/// Where the parts of the card that take something carried were last drawn.
#[derive(Default)]
struct Drawn {
    now: Cell<Option<Bounds<Pixels>>>,
    body: Cell<Option<Bounds<Pixels>>>,
    rows: RefCell<Vec<Bounds<Pixels>>>,
}

pub(crate) struct UpNextCard {
    link: Entity<Link>,
    player: Entity<Player>,
    /// Covers read, by their art.
    pictures: HashMap<Id, Arc<RenderImage>>,
    /// Covers the server reported missing (404): never re-requested in this session.
    missing: HashSet<Id>,
    /// Covers whose fetch failed transiently, with exponential backoff.
    retries: crate::covers::Retries,
    /// Covers currently being fetched.
    in_flight: HashSet<Id>,
    /// In-flight cover fetch tasks, aborted when the card is dropped.
    fetches: crate::covers::FetchBag,
    /// Art IDs currently displayed (now playing + queue entries).
    current_arts: HashSet<Id>,
    /// Last displayed player snapshot, to skip redundant progress-only updates.
    snapshot: Option<CardSnapshot>,
    /// The entries of the last read, and its revision: entries that arrive
    /// in a later one show arriving for a moment.
    seen: HashSet<Id>,
    last_revision: Option<u64>,
    fresh: HashSet<Id>,
    fresh_since: Option<Instant>,
    target: Option<Target>,
    drawn: Rc<Drawn>,
    scroll: ScrollHandle,
    /// The star the pointer is on, and the row it is in.
    star: Option<(usize, u32)>,
    /// The row the pointer is on: its stars take the place of its time.
    /// Kept here, not as a hover style, so that the two are swapped as the
    /// card is built: GPUI does not prepare a box hidden with `display:
    /// none`, and one shown by a hover that changed after that would be
    /// painted unprepared.
    hovered: Option<usize>,
    /// Timer driving the 13 px equalizer bars while playing.
    eq_tick: Option<Task<()>>,
    /// Lying over the page, in a narrow window: the bigger shadow.
    pub over: bool,
    /// Over a colour the page borrows: a thinner fill.
    pub washed: bool,
    _link: Subscription,
    _player: Subscription,
}

impl UpNextCard {
    pub(crate) fn new(link: Entity<Link>, player: Entity<Player>, cx: &mut Context<Self>) -> Self {
        let session = cx.subscribe(
            &link,
            |card: &mut Self, _, event: &LinkEvent, cx| match event {
                LinkEvent::Connected(_) => {
                    card.missing.clear();
                    card.retries = crate::covers::Retries::default();
                    card.read(cx);
                    cx.notify();
                }
                LinkEvent::Restated(_) => {
                    card.retries = crate::covers::Retries::default();
                    card.read(cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        let followed = cx.observe(&player, |card, _, cx| {
            let snap = CardSnapshot::of(card.player.read(cx));
            if card.snapshot.as_ref() == Some(&snap) {
                return;
            }
            card.snapshot = Some(snap);
            card.read(cx);
            cx.notify();
        });
        let mut card = Self {
            link,
            player,
            pictures: HashMap::new(),
            missing: HashSet::new(),
            retries: crate::covers::Retries::default(),
            in_flight: HashSet::new(),
            fetches: crate::covers::FetchBag::default(),
            current_arts: HashSet::new(),
            snapshot: None,
            seen: HashSet::new(),
            last_revision: None,
            fresh: HashSet::new(),
            fresh_since: None,
            target: None,
            drawn: Rc::default(),
            scroll: ScrollHandle::new(),
            star: None,
            hovered: None,
            eq_tick: None,
            over: false,
            washed: false,
            _link: session,
            _player: followed,
        };
        cx.on_release(|card, cx| {
            for (_, image) in card.pictures.drain() {
                crate::covers::drop_unshared(image, None, cx);
            }
        })
        .detach();
        card.snapshot = Some(CardSnapshot::of(card.player.read(cx)));
        card.read(cx);
        card
    }

    /// Drops cached covers that are no longer in `current_arts` once the cache
    /// grows past `MAX_PICTURES`, returning the evicted images for atlas cleanup.
    fn prune_pictures(&mut self) -> Vec<Arc<RenderImage>> {
        prune_unused(&mut self.pictures, &self.current_arts)
    }

    /// Takes what the player last read of Up next: entries that came in a
    /// newer read than the last are fresh. The song on's cover is read too.
    fn read(&mut self, cx: &mut Context<Self>) {
        let player = self.player.read(cx);
        let now = player
            .state
            .as_ref()
            .filter(|state| state.entry_id.is_some())
            .and_then(|state| state.track.as_ref())
            .and_then(|track| track.art_id);
        let read = player.up_next.clone();
        if let Some(art) = now {
            self.current_arts.insert(art);
        }
        self.want(now.into_iter().collect(), cx);
        let Some(read) = read else {
            return;
        };
        let arts: Vec<Id> = read
            .entries
            .iter()
            .filter_map(|entry| entry.track.art_id)
            .collect();
        let mut keep: HashSet<Id> = arts.iter().copied().collect();
        if let Some(art) = now {
            keep.insert(art);
        }
        self.current_arts = keep;
        for old in self.prune_pictures() {
            crate::covers::drop_unshared(old, None, cx);
        }
        // Reconcile queued covers independently of whether the queue revision
        // changed so retry timers and reconnects recover transiently failed art.
        self.want(arts, cx);
        if self.last_revision == Some(read.revision)
            && read.entries.len() == self.seen.len()
            && read
                .entries
                .iter()
                .all(|entry| self.seen.contains(&entry.entry_id))
        {
            return;
        }
        let ids: HashSet<Id> = read.entries.iter().map(|entry| entry.entry_id).collect();
        if self.last_revision.is_some_and(|last| read.revision > last) {
            self.fresh = ids.difference(&self.seen).copied().collect();
            self.fresh_since = Some(Instant::now());
        }
        self.seen = ids;
        self.last_revision = Some(read.revision);
    }

    /// Reads the covers not yet read, at the pixels the song on is drawn at.
    fn want(&mut self, arts: Vec<Id>, cx: &mut Context<Self>) {
        let Some(net) = self.link.read(cx).net() else {
            return;
        };
        let now = Instant::now();
        for art in arts {
            if self.pictures.contains_key(&art)
                || self.missing.contains(&art)
                || self.retries.waits(art, now)
                || !self.in_flight.insert(art)
            {
                continue;
            }
            let px = 88;
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
                this.update(cx, |card, cx| {
                    card.in_flight.remove(&art);
                    match result {
                        Ok((image, _, _)) => {
                            card.retries.read(art);
                            let image = Arc::new(image);
                            for old in
                                take_picture(&mut card.pictures, &card.current_arts, art, image)
                            {
                                crate::covers::drop_unshared(old, None, cx);
                            }
                            cx.notify();
                        }
                        Err(NoArt::Missing) => {
                            card.retries.read(art);
                            card.missing.insert(art);
                        }
                        Err(NoArt::Failed) => {
                            let wait = card.retries.failed(art, Instant::now());
                            cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(wait).await;
                                this.update(cx, |card, cx| {
                                    card.read(cx);
                                    cx.notify();
                                })
                                .ok();
                            })
                            .detach();
                        }
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// Where the pointer, carrying something, would put it.
    fn aim(&mut self, event: &DragMoveEvent<Carried>, cx: &mut Context<Self>) {
        let at = event.event.position;
        let entries = self
            .player
            .read(cx)
            .up_next
            .as_ref()
            .map_or(0, |up_next| up_next.entries.len());
        let drawn = &self.drawn;
        let within = |bounds: Option<Bounds<Pixels>>| bounds.is_some_and(|b| b.contains(&at));
        let target = if !event.bounds.contains(&at) {
            None
        } else if within(drawn.now.get()) {
            // Something carried over the song on goes before everything after it.
            Some(if entries == 0 {
                Target::End
            } else {
                Target::Row {
                    index: 0,
                    after: false,
                }
            })
        } else if within(drawn.body.get()) {
            let rows = drawn.rows.borrow();
            Some(
                rows.iter()
                    .take(entries)
                    .position(|row| row.contains(&at))
                    .and_then(|index| {
                        let row = rows.get(index)?;
                        let middle = row.origin.y + row.size.height / 2.;
                        Some(Target::Row {
                            index,
                            after: at.y > middle,
                        })
                    })
                    .unwrap_or(Target::End),
            )
        } else {
            None
        };
        if target != self.target {
            self.target = target;
            cx.notify();
        }
    }

    fn dropped(&mut self, carried: &Carried, cx: &mut Context<Self>) {
        let Some(at) = self.target.take() else {
            return;
        };
        let place = {
            let player = self.player.read(cx);
            let entries = player
                .up_next
                .as_ref()
                .map_or(&[][..], |up_next| &up_next.entries[..]);
            place_of(at, entries)
        };
        carry::drop(carried, place, &self.player, cx);
        cx.notify();
    }

    /// A cover, or its soft colours for a song with none, `side` points square.
    fn art(&self, track: &TrackSummary, side: f32, radius: f32, window: &Window) -> gpui::Div {
        let picture = match track.art_id {
            Some(art) => self.pictures.get(&art).cloned(),
            None => Some(theme::soft_cached(
                track
                    .album
                    .as_deref()
                    .or(track.title.as_deref())
                    .unwrap_or(""),
                (side * window.scale_factor()).ceil() as u32,
            )),
        };
        div()
            .size(px(side))
            .flex_none()
            .rounded(px(radius))
            .overflow_hidden()
            .bg(theme::fill())
            .children(picture.map(|picture| {
                img(picture)
                    .size(px(side))
                    .rounded(px(radius))
                    .object_fit(ObjectFit::Cover)
            }))
    }

    /// The song on, at the card's head: its menu is the player bar's.
    fn now_row(
        &self,
        track: &TrackSummary,
        eq_seconds: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let drawn = Rc::clone(&self.drawn);
        let menu = crate::menu::mark("card-now", window, cx);
        let summary = track.clone();
        div()
            .id("card-now")
            .relative()
            .flex_none()
            .h(px(60.))
            .mx(px(10.))
            .mb(px(2.))
            .pl(px(8.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(9.))
            .bg(theme::fill())
            .when(menu > 0., |row| row.bg(crate::menu::fill(menu)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let entries = crate::menu::now_menu(&summary, cx);
                    crate::menu::popup("card-now".to_owned(), entries, event.position, window, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, _, _| drawn.now.set(Some(bounds)),
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(self.art(track, 44., 7., window))
            .child(words(track, FontWeight(650.)))
            .child(
                div()
                    .w(px(24.))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(crate::marks::eq(eq_seconds, 13., theme::ink())),
            )
    }

    /// One entry of the list.
    #[allow(
        clippy::too_many_arguments,
        reason = "a row's parts, as the card has them"
    )]
    fn row(
        &self,
        index: usize,
        entry: &QueueEntry,
        last: bool,
        total: u64,
        own: Option<u32>,
        ratings_revision: u64,
        arrived: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let group: SharedString = format!("entry-{index}").into();
        let id = entry.entry_id;
        let track = entry.track.clone();
        let title = track.title.clone().unwrap_or_else(|| "Untitled".to_owned());
        let lined =
            |side: f32| vec![BoxShadow::new(px(0.), px(side), Hsla::from(theme::acc())).inset()];
        let line = match self.target {
            Some(Target::Row { index: at, after }) if at == index => {
                Some(lined(if after { -2. } else { 2. }))
            }
            Some(Target::End) if last => Some(lined(-2.)),
            _ => None,
        };
        let menu_key = format!("entry:{id}");
        let menu = crate::menu::mark(&menu_key, window, cx);
        let drawn = Rc::clone(&self.drawn);
        let recording = track.recording_id;
        let said = track.rating;
        let stars = crate::marks::stars(
            format!("entry-{index}").into(),
            stars_of(own, track.loved),
            self.star
                .filter(|(row, _)| *row == index)
                .map(|(_, star)| star),
            recording.is_some(),
            12.,
            cx,
            move |card: &mut Self, star, over, _| {
                if over {
                    card.star = Some((index, star));
                } else if card.star == Some((index, star)) {
                    card.star = None;
                }
            },
            move |card: &mut Self, star, cx| {
                let Some(recording) = recording else { return };
                card.player.update(cx, |player, cx| {
                    player.click_star(recording, said, Some(ratings_revision), star, cx);
                });
            },
        );
        let rm_group: SharedString = format!("entry-rm-{index}").into();
        let summary = track.clone();
        let carried = Carried::Entries {
            ids: vec![id],
            name: title,
        };
        let hovered = self.hovered == Some(index);
        div()
            .id(("entry", index))
            .group(group.clone())
            .on_hover(cx.listener(move |card, over: &bool, _, cx| {
                let now = if *over {
                    Some(index)
                } else if card.hovered == Some(index) {
                    None
                } else {
                    return;
                };
                if card.hovered != now {
                    card.hovered = now;
                    cx.notify();
                }
            }))
            .relative()
            .h(px(50.))
            .mx(px(-8.))
            .pl(px(8.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(9.))
            .hover(|style| style.bg(theme::fill()))
            .when(menu > 0., |row| row.bg(crate::menu::fill(menu)))
            .when_some(line, |row, line| row.shadow(line))
            .on_drag(carried, carry::lift)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |card, event: &MouseDownEvent, _, cx| {
                    if event.click_count == 2 {
                        card.player.update(cx, |player, cx| player.jump(id, cx));
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let entries = crate::menu::entry_menu(
                        id,
                        &summary,
                        index == 0,
                        index as u64 + 1 == total,
                        cx,
                    );
                    crate::menu::popup(menu_key.clone(), entries, event.position, window, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, _, _| {
                        let mut rows = drawn.rows.borrow_mut();
                        if rows.len() <= index {
                            rows.resize(index + 1, bounds);
                        }
                        if let Some(row) = rows.get_mut(index) {
                            *row = bounds;
                        }
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // A song just added comes up in the accent, and fades into place.
            .when(arrived > 0., |row| {
                row.child(div().absolute().size_full().rounded(px(9.)).bg(gpui::Rgba {
                    a: 0.3 * arrived,
                    ..theme::acc()
                }))
            })
            .child(self.art(&track, 36., 6., window))
            .child(
                words(&track, FontWeight(500.))
                    .when(entry.failed_reason.is_some(), |words| words.opacity(0.5)),
            )
            // The stars take the place of the time while the pointer is on the row.
            .when(hovered, |row| row.child(div().flex_none().child(stars)))
            .when(!hovered, |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(px(12.5))
                        .text_color(theme::dim())
                        .font_features(crate::bar::tabular())
                        .child(track.duration_ms.map(clock).unwrap_or_default()),
                )
            })
            .child(
                div()
                    .id(("entry-rm", index))
                    .group(rm_group.clone())
                    .size(px(24.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .hover(|style| style.bg(theme::fill2()))
                    // Not a double-click on the row, nor a drag of it.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |card, _, _, cx| {
                        card.player.update(cx, |player, cx| player.remove(id, cx));
                    }))
                    .child(
                        icon("close", 9., theme::dim())
                            .group_hover(rm_group, |style| style.text_color(theme::ink())),
                    ),
            )
    }
}

/// A song's title over its artist, each cut short to fit.
fn words(track: &TrackSummary, weight: FontWeight) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .truncate()
                .font_weight(weight)
                .child(track.title.clone().unwrap_or_else(|| "Untitled".to_owned())),
        )
        .child(
            div()
                .truncate()
                .text_size(px(12.5))
                .text_color(theme::dim())
                .child(
                    track
                        .artist
                        .clone()
                        .unwrap_or_else(|| "Unknown artist".to_owned()),
                ),
        )
}

/// A quiet two lines in the middle of the card, the second's `+` stronger.
fn empty(first: &str, second: &str) -> gpui::Div {
    let mut highlights = Vec::new();
    if let Some(at) = second.find('+') {
        highlights.push((
            at..at + 1,
            gpui::HighlightStyle {
                color: Some(theme::dim().into()),
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
        ));
    }
    div()
        .my(px(48.))
        .flex()
        .flex_col()
        .items_center()
        .text_center()
        .line_height(gpui::relative(1.6))
        .text_color(theme::faint())
        .child(first.to_owned())
        .child(gpui::StyledText::new(second.to_owned()).with_highlights(highlights))
}

impl Render for UpNextCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let player = self.player.read(cx);
        let state = player.state.clone();
        let now = state
            .as_ref()
            .filter(|state| state.entry_id.is_some())
            .and_then(|state| state.track.clone());
        let up_next = player.up_next.clone();
        let ratings_revision = up_next.as_ref().map_or(0, |queue| queue.ratings_revision);
        let entries = up_next
            .as_ref()
            .map_or_else(Vec::new, |up_next| up_next.entries.clone());
        let total = up_next.as_ref().map_or(0, |up_next| up_next.total);
        let more = total.saturating_sub(entries.len() as u64);
        let repeat = state.as_ref().map_or(Repeat::Off, |state| state.repeat);
        let shuffle = state.as_ref().is_some_and(|state| state.shuffle);
        let playing = player.playing();
        let eq_seconds = player.eq_seconds();
        // How long what is listed has left to play, with the rest of the song on.
        let ahead: u64 = entries
            .iter()
            .map(|entry| entry.track.duration_ms.unwrap_or(0))
            .sum();
        let here = state
            .as_ref()
            .and_then(|state| state.duration_ms)
            .unwrap_or(0)
            .saturating_sub(player.position());
        let left = ahead + here;
        let owns: Vec<Option<u32>> = entries
            .iter()
            .map(|entry| {
                entry
                    .track
                    .recording_id
                    .map_or(entry.track.rating, |recording| {
                        player.own(recording, entry.track.rating, Some(ratings_revision))
                    })
            })
            .collect();
        let carrying = carry::carried(cx).is_some();
        if !carrying {
            self.target = None;
        }
        let eq_moving = now.is_some() && playing && !crate::platform::reduce_motion();
        let mut animating = false;

        // Songs just added, in the accent for a moment.
        let arrived = match self.fresh_since {
            Some(since) if !crate::platform::reduce_motion() => {
                let t =
                    crate::moves::progress(crate::moves::since(since, Instant::now()), ARRIVED_MS);
                if t >= 1. {
                    self.fresh.clear();
                    self.fresh_since = None;
                    0.
                } else {
                    animating = true;
                    1. - crate::shell::bezier(0., 0., 0.58, 1., t)
                }
            }
            _ => {
                self.fresh.clear();
                self.fresh_since = None;
                0.
            }
        };

        let drawn = Rc::clone(&self.drawn);
        let mut body = div()
            .id("card-body")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .pt(px(12.))
            .px(px(18.))
            .pb(px(18.))
            .child(
                canvas(
                    move |bounds, _, _| drawn.body.set(Some(bounds)),
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            );
        if now.is_none() {
            body = body.child(empty(
                "Nothing is playing.",
                "Double-click a song, or press Play on anything.",
            ));
        } else if entries.is_empty() {
            body = body.child(empty(
                "Nothing after this song.",
                "Press + on any song, or drag songs and records here.",
            ));
            if carrying {
                let on = self.target == Some(Target::End);
                let acc = theme::acc();
                body = body.child(
                    div()
                        .h(px(50.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.))
                        .text_size(px(13.))
                        .font_weight(FontWeight(600.))
                        .text_color(if on { theme::ink() } else { theme::dim() })
                        .when(on, |slot| slot.bg(gpui::Rgba { a: 0.22, ..acc }))
                        .shadow(vec![
                            BoxShadow::new(
                                px(0.),
                                px(0.),
                                Hsla::from(if on {
                                    acc
                                } else {
                                    gpui::Rgba { a: 0.5, ..acc }
                                }),
                            )
                            .spread_radius(px(1.5))
                            .inset(),
                        ])
                        .child("Drop here to play next"),
                );
            }
        } else {
            body = body.child(
                div()
                    .mb(px(6.))
                    .text_size(px(12.5))
                    .text_color(theme::faint())
                    .child(if shuffle {
                        "Next, in shuffled order"
                    } else {
                        "Next"
                    }),
            );
            self.drawn.rows.borrow_mut().truncate(entries.len());
            let mut list = div().flex().flex_col();
            for (index, entry) in entries.iter().enumerate() {
                let shown = if self.fresh.contains(&entry.entry_id) {
                    arrived
                } else {
                    0.
                };
                let own = owns.get(index).copied().flatten();
                let last = index + 1 == entries.len();
                list = list.child(self.row(
                    index,
                    entry,
                    last,
                    total,
                    own,
                    ratings_revision,
                    shown,
                    window,
                    cx,
                ));
            }
            body = body.child(list);
            if more > 0 {
                body = body.child(
                    div()
                        .mt(px(10.))
                        .text_size(px(13.))
                        .text_color(theme::faint())
                        .child(format!("and {}", plural(more, "more song"))),
                );
            }
        }

        let foot = (now.is_some() && total > 0).then(|| {
            let after = match repeat {
                Repeat::All => " · then round again".to_owned(),
                Repeat::One => " · after this song stops repeating".to_owned(),
                Repeat::Off if more == 0 => format!(" · {} left", length(left)),
                Repeat::Off => String::new(),
            };
            div()
                .relative()
                .flex_none()
                .pt(px(12.))
                .px(px(18.))
                .pb(px(14.))
                .text_size(px(13.))
                .text_color(theme::dim())
                .font_features(crate::bar::tabular())
                .child(
                    div()
                        .absolute()
                        .top(px(-1.))
                        .left_0()
                        .right_0()
                        .h(px(1.))
                        .bg(theme::line()),
                )
                .child(format!("{} to come{after}", plural(total, "song")))
        });

        // Its edge: a hairline, or carrying something, the accent, showing
        // it can take it; and its shadow, bigger lying over the page.
        let (ring, ring_color) = if carrying {
            (
                1.5,
                gpui::Rgba {
                    a: 0.6,
                    ..theme::acc()
                },
            )
        } else {
            (1., theme::line())
        };
        let shadow = if self.over && !carrying {
            css_shadow(24., 70., gpui::rgba(0x0000006b))
        } else {
            css_shadow(10., 30., gpui::rgba(0x00000033))
        };
        if animating {
            self.eq_tick = None;
            window.request_animation_frame();
        } else if eq_moving {
            if self.eq_tick.is_none() {
                self.eq_tick = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(EQ_TICK).await;
                    this.update(cx, |card, cx| {
                        card.eq_tick = None;
                        cx.notify();
                    })
                    .ok();
                }));
            }
        } else {
            self.eq_tick = None;
        }
        let height = f32::from(window.viewport_size().height) - TOP - BOTTOM;

        div()
            .id("up-next")
            .occlude()
            .relative()
            .size_full()
            .children(outside(shadow, WIDTH, height, RADIUS))
            .child(
                div()
                    .absolute()
                    .top(px(-ring))
                    .left(px(-ring))
                    .w(px(WIDTH + 2. * ring))
                    .h(px(height + 2. * ring))
                    .rounded(px(RADIUS + ring))
                    .border(px(ring))
                    .border_color(ring_color),
            )
            .on_drag_move(cx.listener(|card, event: &DragMoveEvent<Carried>, _, cx| {
                card.aim(event, cx);
            }))
            .on_drop(cx.listener(|card, carried: &Carried, _, cx| card.dropped(carried, cx)))
            // What is behind the card, blurred, under its fill (`frost.rs`).
            .child(
                gpui::canvas(
                    |_, _, _| {},
                    |bounds, (), window, _| crate::frost::paint(bounds, RADIUS, window),
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(px(RADIUS))
                    .bg(gpui::Rgba {
                        a: if self.washed { 0.66 } else { 0.86 },
                        ..theme::surf()
                    })
                    .child(self.head(cx))
                    .when_some(now.clone(), |card, now| {
                        card.child(
                            div()
                                .flex_none()
                                .pt(px(10.))
                                .px(px(18.))
                                .pb(px(6.))
                                .text_size(px(12.5))
                                .text_color(theme::faint())
                                .child("Now playing"),
                        )
                        .child(self.now_row(&now, eq_seconds, window, cx))
                    })
                    .child(body)
                    .children(foot),
            )
    }
}

impl UpNextCard {
    /// The card's head: its name, and the button that puts it away.
    fn head(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .pt(px(14.))
            .pr(px(12.))
            .pb(px(4.))
            .pl(px(18.))
            .child(div().flex_1().child(crate::tracked::tracked(
                "Up next",
                18.,
                18. * 1.4,
                FontWeight(650.),
                theme::ink(),
                -0.01,
            )))
            .child(
                div()
                    .id("up-next-close")
                    .group("up-next-close")
                    .size(px(28.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|style| style.bg(theme::fill()))
                    .on_click(cx.listener(|card, _, _, cx| {
                        card.player
                            .update(cx, |player, cx| player.toggle_card(Some(false), cx));
                    }))
                    .child(
                        icon("close", 10., theme::dim())
                            .group_hover("up-next-close", |style| style.text_color(theme::ink())),
                    ),
            )
    }
}

/// A shadow `y` points down with CSS's `blur` radius. CSS blurs by a
/// Gaussian of half its blur radius; GPUI's blur radius is that Gaussian's
/// own deviation, so it takes half.
fn css_shadow(y: f32, blur: f32, color: gpui::Rgba) -> BoxShadow {
    BoxShadow::new(px(0.), px(y), Hsla::from(color)).blur_radius(px(blur / 2.))
}

/// How tall each row of a corner's sliver is: a pixel of a Retina screen.
const SLIVER_ROW: f32 = 0.5;

/// A box's outer shadow, drawn only outside it, as CSS draws one: GPUI
/// draws a shadow under the box too, which would darken a see-through
/// card from within. The shadow is clipped to four strips round the box,
/// as far as GPUI draws it (three deviations of its blur past its shape),
/// and to the slivers outside its rounded corners, in rows a pixel tall
/// that each run in to where the corner's curve meets the row's outer
/// side; what of a row lies inside the curve is under the card's edge.
fn outside(shadow: BoxShadow, width: f32, height: f32, radius: f32) -> Vec<gpui::Div> {
    let reach = 3. * f32::from(shadow.blur_radius)
        + f32::from(shadow.spread_radius)
        + f32::from(shadow.offset.x)
            .abs()
            .max(f32::from(shadow.offset.y).abs());
    let strip = |left: f32, top: f32, w: f32, h: f32| {
        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(w))
            .h(px(h))
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left(px(-left))
                    .top(px(-top))
                    .w(px(width))
                    .h(px(height))
                    .rounded(px(radius))
                    .shadow(vec![shadow.clone()]),
            )
    };
    let mut strips = vec![
        strip(-reach, -reach, width + 2. * reach, reach),
        strip(-reach, height, width + 2. * reach, reach),
        strip(-reach, 0., reach, height),
        strip(width, 0., reach, height),
    ];
    let mut y = 0.;
    while y < radius {
        let h = SLIVER_ROW.min(radius - y);
        let rise = radius - y;
        let x = radius - (radius * radius - rise * rise).max(0.).sqrt();
        if x > 0.01 {
            let bottom = height - y - h;
            strips.push(strip(0., y, x, h));
            strips.push(strip(width - x, y, x, h));
            strips.push(strip(0., bottom, x, h));
            strips.push(strip(width - x, bottom, x, h));
        }
        y += SLIVER_ROW;
    }
    strips
}

/// Prunes cached covers not in `keep` once `pictures` grows past `MAX_PICTURES`.
fn prune_unused(
    pictures: &mut HashMap<Id, Arc<RenderImage>>,
    keep: &HashSet<Id>,
) -> Vec<Arc<RenderImage>> {
    if pictures.len() <= MAX_PICTURES {
        return Vec::new();
    }
    let drop_ids: Vec<Id> = pictures
        .keys()
        .copied()
        .filter(|id| !keep.contains(id))
        .collect();
    let mut evicted = Vec::with_capacity(drop_ids.len());
    for id in drop_ids {
        if let Some(image) = pictures.remove(&id) {
            evicted.push(image);
        }
    }
    evicted
}

/// Inserts a fetched cover if `art` is still in `keep`, otherwise returns it
/// immediately for atlas cleanup so a late completion never repopulates a
/// pruned queue entry.
fn take_picture(
    pictures: &mut HashMap<Id, Arc<RenderImage>>,
    keep: &HashSet<Id>,
    art: Id,
    image: Arc<RenderImage>,
) -> Vec<Arc<RenderImage>> {
    if !keep.contains(&art) {
        return vec![image];
    }
    let mut dropped = Vec::new();
    if let Some(old) = pictures.insert(art, image) {
        dropped.push(old);
    }
    dropped.extend(prune_unused(pictures, keep));
    dropped
}

#[cfg(test)]
mod tests {
    use gpui::AppContext;

    use super::*;

    fn entry(id: u8) -> QueueEntry {
        QueueEntry {
            entry_id: Id::from_bytes([id; 16]),
            track: TrackSummary {
                appearance_id: Id::from_bytes([id + 100; 16]),
                recording_id: None,
                release_id: None,
                title: None,
                artist: None,
                album: None,
                disc_number: None,
                track_number: None,
                duration_ms: None,
                art_id: None,
                rating: None,
                loved: false,
            },
            failed_reason: None,
        }
    }

    #[test]
    fn a_drop_goes_after_the_row_above_it_or_next_at_the_top() {
        let entries: Vec<QueueEntry> = (1..=3).map(entry).collect();
        let after = |id: u8| QueuePlace::After {
            entry_id: Id::from_bytes([id; 16]),
        };
        let row = |index, after| Target::Row { index, after };
        assert_eq!(place_of(row(0, false), &entries), QueuePlace::Next);
        assert_eq!(place_of(row(0, true), &entries), after(1));
        assert_eq!(place_of(row(2, false), &entries), after(2));
        assert_eq!(place_of(Target::End, &entries), QueuePlace::End);
        // With nothing after the song on, the open space plays it next.
        assert_eq!(place_of(Target::End, &[]), QueuePlace::Next);
    }

    #[test]
    fn pruning_drops_stale_covers_handles_late_completions_and_large_queues() {
        let mut pictures = HashMap::new();
        let kept = Id::from_bytes([1; 16]);
        let late = Id::from_bytes([250; 16]);
        let make = || {
            Arc::new(RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::new(1, 1),
            )]))
        };
        for i in 1..=(MAX_PICTURES + 10) {
            let id = Id::from_bytes([i as u8; 16]);
            pictures.insert(id, make());
        }
        let keep = HashSet::from([kept]);
        let evicted = prune_unused(&mut pictures, &keep);
        assert_eq!(evicted.len(), MAX_PICTURES + 9);
        assert_eq!(pictures.len(), 1);
        assert!(pictures.contains_key(&kept));

        // A late completion for an art ID no longer in the queue is rejected
        // and returned for atlas cleanup instead of repopulating `pictures`.
        let rejected = take_picture(&mut pictures, &keep, late, make());
        assert_eq!(rejected.len(), 1);
        assert!(!pictures.contains_key(&late));

        // A live queue larger than MAX_PICTURES keeps all of its current arts.
        let large_keep: HashSet<Id> = (1..=(MAX_PICTURES + 20))
            .map(|i| Id::from_bytes([i as u8; 16]))
            .collect();
        for &id in &large_keep {
            assert!(take_picture(&mut pictures, &large_keep, id, make()).is_empty() || id == kept);
        }
        assert_eq!(pictures.len(), MAX_PICTURES + 20);
    }

    #[gpui::test]
    fn snapshot_changes_on_optimistic_queue_move(cx: &mut gpui::TestAppContext) {
        let link = cx.update(crate::link::unlinked);
        let player = cx.new(|cx| Player::new(&link, cx));
        let mut e1 = entry(1);
        let mut e2 = entry(2);
        e1.track.duration_ms = Some(180_000);
        e2.track.duration_ms = Some(180_000);
        let e2_id = e2.entry_id;
        let before = player.update(cx, |player, _| {
            player.up_next = Some(crate::player::UpNext {
                revision: 4,
                ratings_revision: 1,
                total: 2,
                entries: vec![e1, e2],
            });
            CardSnapshot::of(player)
        });
        let after = player.update(cx, |player, cx| {
            player.move_entries(vec![e2_id], QueuePlace::Next, cx);
            CardSnapshot::of(player)
        });
        assert_ne!(before, after);
    }

    #[gpui::test]
    fn read_retries_queued_art_when_queue_revision_is_unchanged(cx: &mut gpui::TestAppContext) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        cx.executor().allow_parking();
        let rt = crate::net::rt();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = Arc::clone(&requests);
        let address = rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
            let addr = listener.local_addr().ok()?;
            tokio::spawn(async move {
                let mut body = Vec::new();
                let img = image::RgbImage::from_pixel(32, 32, image::Rgb([40, 80, 120]));
                let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut body, 90)
                    .encode_image(&img);
                while let Ok((mut stream, _)) = listener.accept().await {
                    let reqs = Arc::clone(&server_requests);
                    let body = body.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 1024];
                        let Ok(n) = stream.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        let attempt = reqs.fetch_add(1, Ordering::SeqCst);
                        let reply = if attempt == 0 {
                            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n"
                                .to_owned()
                        } else {
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                        };
                        let _ = stream.write_all(reply.as_bytes()).await;
                        if attempt > 0 {
                            let _ = stream.write_all(&body).await;
                        }
                    });
                }
            });
            Some(addr)
        });
        assert!(address.is_some(), "art test server failed to bind");
        let Some(address) = address else { return };

        let art = Id::from_bytes([77; 16]);
        let mut queued = entry(1);
        queued.track.art_id = Some(art);

        let (send, _calls) = tokio::sync::mpsc::channel::<String>(16);
        let (_answer, receive) = tokio::sync::mpsc::channel(16);
        let _guard = rt.enter();
        let client = Arc::new(ritornel_client::Client::over_memory(send, receive));
        let net = Arc::new(crate::net::Net::satellite(client, true).with_art(address, vec![88]));
        let link = cx.update(crate::link::unlinked);
        link.update(cx, |link, cx| {
            link.state = crate::link::State::Connected(Arc::clone(&net));
            cx.notify();
        });
        let player = cx.new(|cx| Player::new(&link, cx));
        player.update(cx, |player, _| {
            player.up_next = Some(crate::player::UpNext {
                revision: 9,
                ratings_revision: 1,
                total: 1,
                entries: vec![queued],
            });
        });
        let card = cx.new(|cx| UpNextCard::new(link.clone(), player.clone(), cx));
        // Wait for the initial 503 failure to finish.
        let deadline = Instant::now() + Duration::from_secs(10);
        while card.read_with(cx, |card, _| {
            requests.load(Ordering::SeqCst) < 1 || !card.in_flight.is_empty()
        }) {
            assert!(Instant::now() < deadline, "initial fetch did not finish");
            cx.run_until_parked();
            // sleep: yield to Tokio background server for the 503 response.
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!card.read_with(cx, |card, _| card.pictures.contains_key(&art)));

        // Simulate retry timer firing with an unchanged queue revision/entries.
        card.update(cx, |card, cx| {
            card.retries = crate::covers::Retries::default();
            card.read(cx);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !card.read_with(cx, |card, _| card.pictures.contains_key(&art)) {
            assert!(
                Instant::now() < deadline,
                "queued cover was not retried on unchanged queue"
            );
            cx.run_until_parked();
            // sleep: yield to Tokio background server for the 200 retry response.
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }
}
