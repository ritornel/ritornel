//! The bar's widget for where the music plays and how loud: 164 points
//! wide in every state, before Up next, with the speaker icon at its right
//! end, and the picker that rises from it ("Play on" in
//! docs/internals/desktop.md). What it shows is `places::look`.
//!
//! On this computer it is the icon alone, or under `always` a slider led
//! by a volume icon. Playing elsewhere it lights up: an accent tint from
//! end to end, the place's name, faded at its end rather than cut short,
//! and that place's volume under it. A place in trouble wears its
//! trouble's colour, amber out of reach and red when its output failed, and
//! its slider dims and stops answering.
//!
//! The slider sends about ten times a second while it is dragged and once
//! more when let go, and holds its own level against the place's reports
//! meanwhile. A scroll moves it only when it began with the pointer at
//! rest on the widget, never on a touchpad's momentum.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AppContext, Bounds, Context, Entity, FontWeight, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, ScrollDelta,
    ScrollEvent, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, TouchPhase,
    Window, canvas, div, linear_color_stop, linear_gradient, prelude::FluentBuilder, px,
};

use ritornel_contract::Id;

use crate::icons::icon;
use crate::places::{Look, Places, line, plays, waves};
use crate::player::Player;
use crate::theme;
use crate::theme::Status;

/// How big a place's mark is beside its line in the picker.
const PICKER_MARK: f32 = 8.;

/// The widget's width, in every state.
pub(crate) const WIDTH: f32 = 164.;
/// Its height: two lines, the name and the slider.
const HEIGHT: f32 = 36.;
/// The speaker icon's button, at the right end.
const ICON: f32 = 28.;
/// The picker's width.
const PICKER: f32 = 288.;
/// The room the place's name has: the widget less the icon's button and
/// the name's insets.
const NAME_ROOM: f32 = WIDTH - ICON - 12.;
/// The slider's thumb, and the thumb under the pointer or held.
const THUMB: f32 = 6.;
const THUMB_HELD: f32 = 8.;
/// How far above and below the slider a press still takes it.
const GRAB: f32 = 4.;

/// How often a dragged slider sends its level.
const SEND_EVERY: Duration = Duration::from_millis(100);
/// How long the pointer rests on the widget before a scroll moves the volume.
const REST: Duration = Duration::from_millis(250);
/// A pause in scrolling this long ends a gesture.
const GESTURE_GAP: Duration = Duration::from_millis(200);

/// Lighting up, going out, and turning to the warning colour.
const LIGHT_UP: Duration = Duration::from_millis(240);
const GO_OUT: Duration = Duration::from_millis(160);
const NAME_AFTER: Duration = Duration::from_millis(60);
const NAME_OUT: Duration = Duration::from_millis(80);
const WARN: Duration = Duration::from_millis(300);
/// How far the name rises as it comes.
const NAME_RISE: f32 = 3.;

/// When a value may be sent, at most every `SEND_EVERY`.
#[derive(Debug, Default)]
pub(crate) struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    /// How long until a value may be sent; `None` when it may be now.
    pub(crate) fn wait(&self, now: Instant) -> Option<Duration> {
        let last = self.last?;
        SEND_EVERY
            .checked_sub(now.duration_since(last))
            .filter(|left| !left.is_zero())
    }

    pub(crate) fn sent(&mut self, now: Instant) {
        self.last = Some(now);
    }
}

/// Whether a scroll on the widget moves the volume: only a gesture that
/// began with the pointer resting there, and never the momentum after it.
#[derive(Debug, Default)]
pub(crate) struct ScrollGate {
    /// When the pointer last moved on the widget.
    rest_from: Option<Instant>,
    /// Whether the gesture under way moves the volume, once decided.
    gesture: Option<bool>,
    /// Whether the fingers have lifted, so what comes is momentum.
    momentum: bool,
    last: Option<Instant>,
}

impl ScrollGate {
    pub(crate) fn moved(&mut self, now: Instant) {
        self.rest_from = Some(now);
    }

    pub(crate) fn left(&mut self) {
        self.rest_from = None;
        self.gesture = None;
    }

    /// One scroll event, `momentum` when the system says it is. Whether it
    /// moves the volume.
    pub(crate) fn scroll(&mut self, phase: TouchPhase, momentum: bool, now: Instant) -> bool {
        if self
            .last
            .is_none_or(|last| now.duration_since(last) >= GESTURE_GAP)
        {
            self.gesture = None;
            self.momentum = false;
        }
        self.last = Some(now);
        let rested = self
            .rest_from
            .is_some_and(|from| now.duration_since(from) >= REST);
        match phase {
            TouchPhase::Started => {
                self.momentum = false;
                let takes = rested && !momentum;
                self.gesture = Some(takes);
                takes
            }
            TouchPhase::Ended => {
                let took = self.gesture == Some(true) && !momentum;
                self.gesture = None;
                self.momentum = true;
                took
            }
            TouchPhase::Cancelled => {
                self.gesture = None;
                false
            }
            TouchPhase::Moved => {
                if momentum || self.momentum {
                    return false;
                }
                *self.gesture.get_or_insert(rested)
            }
        }
    }
}

/// How far a scroll moves the volume: a wheel's notch a twentieth, a
/// touchpad's 200 points the whole way. Up is louder.
pub(crate) fn scrolled(delta: ScrollDelta) -> f64 {
    match delta {
        ScrollDelta::Lines(lines) => f64::from(lines.y) * 0.05,
        ScrollDelta::Pixels(pixels) => f64::from(f32::from(pixels.y)) / 200.,
    }
}

/// The widget lighting up and going out.
#[derive(Debug, Default)]
pub(crate) struct Light {
    lit: bool,
    warn: bool,
    /// When it last lit or went out; `None` as it first showed.
    since: Option<Instant>,
    warn_since: Option<Instant>,
}

/// Where the light is, as drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Shown {
    /// How much of the widget the tint covers, from its right end, and how
    /// strong it is (only that, with Reduce motion).
    pub reach: f32,
    pub tint: f32,
    /// The name's opacity, and how far below its place it is.
    pub name: f32,
    pub drop: f32,
    /// How far the colour has gone to the warning one.
    pub warn: f32,
    pub moving: bool,
}

impl Light {
    pub(crate) fn follow(&mut self, lit: bool, warn: bool, now: Instant) {
        if lit != self.lit {
            self.lit = lit;
            self.since = Some(now);
        }
        if warn != self.warn {
            self.warn = warn;
            self.warn_since = Some(now);
        }
    }

    pub(crate) fn shown(&self, now: Instant, still: bool) -> Shown {
        let elapsed = |at: Option<Instant>| at.map(|at| now.duration_since(at));
        let part =
            |gone: Duration, of: Duration| (gone.as_secs_f32() / of.as_secs_f32()).clamp(0., 1.);
        let warn = elapsed(self.warn_since).map_or(1., |gone| part(gone, WARN));
        let warn_moving = warn < 1.;
        let warn = if self.warn { warn } else { 1. - warn };
        let gone = elapsed(self.since);
        let (light, name, moving) = match (self.lit, gone) {
            (true, None) => (1., 1., false),
            (false, None) => (0., 0., false),
            (true, Some(gone)) => {
                let t = crate::shell::bezier(0.2, 0.7, 0.2, 1., part(gone, LIGHT_UP));
                let name = part(gone.saturating_sub(NAME_AFTER), LIGHT_UP - NAME_AFTER);
                (
                    t,
                    crate::shell::bezier(0.2, 0.7, 0.2, 1., name),
                    gone < LIGHT_UP,
                )
            }
            (false, Some(gone)) => (
                1. - crate::upnext::cubic_out(part(gone, GO_OUT)),
                1. - part(gone, NAME_OUT),
                gone < GO_OUT,
            ),
        };
        let reach = if still {
            1.
        } else {
            (ICON + (WIDTH - ICON) * light) / WIDTH
        };
        Shown {
            reach,
            tint: light,
            name,
            drop: if still { 0. } else { NAME_RISE * (1. - name) },
            warn,
            moving: moving || warn_moving,
        }
    }
}

/// How wide `name` is as the widget sets it: 12 points, weight 500.
fn name_width(name: &str, window: &Window) -> f32 {
    let mut font = window.text_style().font();
    font.family = ".SystemUIFont".into();
    font.weight = FontWeight(500.);
    let run = gpui::TextRun {
        len: name.len(),
        font,
        color: theme::ink().into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    f32::from(
        window
            .text_system()
            .shape_line(SharedString::from(name.to_owned()), px(12.), &[run], None)
            .width,
    )
}

/// What hovering the widget shows: the whole name, and in trouble the
/// whole sentence, in the trouble's colour.
struct HoverCard {
    name: SharedString,
    more: Option<(SharedString, Status)>,
}

impl Render for HoverCard {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(300.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(10.))
            .bg(theme::raised())
            .shadow(crate::search::shadow())
            .text_size(px(12.5))
            .line_height(px(17.))
            .child(
                div()
                    .font_weight(FontWeight(600.))
                    .text_color(theme::ink())
                    .child(self.name.clone()),
            )
            .children(
                self.more
                    .clone()
                    .map(|(more, status)| div().text_color(status.text()).child(more)),
            )
    }
}

pub(crate) struct Speaker {
    places: Entity<Places>,
    player: Entity<Player>,
    /// The level shown while the slider is held or a scroll moves it.
    held: Option<f64>,
    /// Whether the pointer holds the slider.
    holding: bool,
    /// The player's generation the level held belongs to (`Player::generation`).
    gesture: Option<u64>,
    /// Whether the level held is still to be sent.
    unsent: bool,
    /// The level last shown, kept while a place taken over is read.
    last_level: Option<f64>,
    throttle: Throttle,
    /// Sends the last level once the throttle allows.
    flush: Option<Task<()>>,
    /// Lets the level go back to the place's after a scroll.
    scroll_ends: Option<Task<()>>,
    gate: ScrollGate,
    light: Light,
    /// The status of the trouble last shown, kept while its colour fades.
    trouble: Status,
    /// The name last lit, kept while it goes out.
    lit_name: Option<SharedString>,
    slider: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Where the speaker icon is: a click on it is its own, not one outside the picker.
    icon_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// When the picker opened, for its moving bars.
    picker_opened: Option<Instant>,
    /// The picker's list, scrolled to the row the keys are on.
    scroll: gpui::ScrollHandle,
    scrolled_to: Option<Id>,
    _places: Subscription,
    _player: Subscription,
}

impl Speaker {
    pub(crate) fn new(
        places: Entity<Places>,
        player: Entity<Player>,
        cx: &mut Context<Self>,
    ) -> Self {
        let followed = cx.observe(&places, |_, _, cx| cx.notify());
        let heard = cx.observe(&player, |_, _, cx| cx.notify());
        Self {
            places,
            player,
            held: None,
            holding: false,
            gesture: None,
            unsent: false,
            last_level: None,
            throttle: Throttle::default(),
            flush: None,
            scroll_ends: None,
            gate: ScrollGate::default(),
            light: Light::default(),
            trouble: Status::Need,
            lit_name: None,
            slider: Rc::default(),
            icon_bounds: Rc::default(),
            picker_opened: None,
            scroll: gpui::ScrollHandle::new(),
            scrolled_to: None,
            _places: followed,
            _player: heard,
        }
    }

    /// Shows `volume` at once, and sends it now or as soon as the throttle
    /// allows. The level belongs to the place played on as it began: a
    /// change of place or session ends it unsent.
    fn want(&mut self, volume: f64, cx: &mut Context<Self>) {
        let generation = self.player.read(cx).generation();
        if self.gesture.is_some_and(|began| began != generation) {
            self.cancel(cx);
        }
        self.gesture = Some(generation);
        self.held = Some(volume.clamp(0., 1.));
        self.unsent = true;
        self.pump(cx);
        cx.notify();
    }

    /// Sends the level held if the throttle allows, or waits until it does.
    fn pump(&mut self, cx: &mut Context<Self>) {
        if !self.unsent {
            return;
        }
        match self.throttle.wait(cx.background_executor().now()) {
            None => self.send(cx),
            Some(wait) if self.flush.is_none() => {
                self.flush = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    this.update(cx, |speaker, cx| {
                        speaker.flush = None;
                        speaker.pump(cx);
                    })
                    .ok();
                }));
            }
            Some(_) => {}
        }
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let Some(volume) = self.held else { return };
        if self.gesture != Some(self.player.read(cx).generation()) {
            self.cancel(cx);
            return;
        }
        self.flush = None;
        self.unsent = false;
        self.throttle.sent(cx.background_executor().now());
        self.player
            .update(cx, |player, cx| player.set_volume(volume, cx));
    }

    /// Lets the slider go: the last level is sent once more, and from then
    /// on the place's own reports move it.
    fn let_go(&mut self, cx: &mut Context<Self>) {
        self.send(cx);
        self.cancel(cx);
    }

    /// Mutes the place, or unmutes it. A scroll still under way sends its
    /// level first, so that it cannot undo this.
    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        if self.held.is_some() {
            self.let_go(cx);
        }
        let muted = self
            .player
            .read(cx)
            .state
            .as_ref()
            .is_some_and(|state| state.muted);
        self.player
            .update(cx, |player, cx| player.set_muted(!muted, cx));
    }

    /// Ends a drag or a scroll without sending what it holds.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.held = None;
        self.holding = false;
        self.unsent = false;
        self.gesture = None;
        self.flush = None;
        self.scroll_ends = None;
        cx.notify();
    }

    /// The level at `x`, along the line inside the slider's inset.
    fn fraction_at(&self, x: Pixels) -> Option<f64> {
        let bounds = self.slider.get()?;
        let width = f32::from(bounds.size.width) - THUMB_HELD;
        let along = f32::from(x - bounds.origin.x) - THUMB_HELD / 2.;
        (width > 0.).then(|| f64::from((along / width).clamp(0., 1.)))
    }

    fn scroll(&mut self, event: &ScrollEvent, answers: bool, cx: &mut Context<Self>) {
        let momentum = crate::platform::scroll_details()
            .is_some_and(|details| details.momentum != crate::platform::ScrollPhase::None);
        if !self
            .gate
            .scroll(event.touch_phase, momentum, Instant::now())
            || !answers
        {
            return;
        }
        let by = scrolled(event.delta);
        let Some(from) = self.held.or_else(|| {
            self.player
                .read(cx)
                .state
                .as_ref()
                .map(|state| state.volume)
        }) else {
            return;
        };
        if by != 0. {
            self.want(from + by, cx);
        }
        // Once the scroll ends, the place's own reports move it again.
        self.scroll_ends = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GESTURE_GAP).await;
            this.update(cx, |speaker, cx| {
                speaker.scroll_ends = None;
                if !speaker.holding && speaker.held.is_some() {
                    speaker.let_go(cx);
                }
            })
            .ok();
        }));
    }

    /// The slider: a line that fills to the level, and while it answers
    /// takes a click or a drag.
    fn slider(
        &self,
        volume: f64,
        answers: bool,
        dimmed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bounds = Rc::clone(&self.slider);
        let this = cx.entity().downgrade();
        let holding = self.holding;
        div()
            .id("volume")
            .group("volume")
            .flex_1()
            .min_w_0()
            // The line is inset by the thumb's half, so the thumb stays
            // within what takes the pointer at either end.
            .py(px(5.))
            .px(px(THUMB_HELD / 2.))
            .relative()
            .when(dimmed, |slider| slider.opacity(0.4))
            .child(
                div()
                    .relative()
                    .h(px(3.))
                    .child(
                        div()
                            .size_full()
                            .rounded(px(2.))
                            .overflow_hidden()
                            .bg(theme::fill2())
                            .child(
                                div()
                                    .h_full()
                                    .w(gpui::relative(volume as f32))
                                    .rounded(px(2.))
                                    .bg(if holding { theme::acc() } else { theme::ink() })
                                    .when(answers, |level| {
                                        level.group_hover("volume", |style| style.bg(theme::acc()))
                                    }),
                            ),
                    )
                    // The thumb, which grows a little under the pointer and while held.
                    .child({
                        let side = if holding { THUMB_HELD } else { THUMB };
                        div()
                            .id("volume-thumb")
                            .absolute()
                            .top(px(1.5 - side / 2.))
                            .left(gpui::relative(volume as f32))
                            .ml(px(-side / 2.))
                            .size(px(side))
                            .rounded_full()
                            .bg(if holding { theme::acc() } else { theme::ink() })
                            .when(answers && !holding, |thumb| {
                                thumb.group_hover("volume", |style| {
                                    style
                                        .size(px(THUMB_HELD))
                                        .top(px(1.5 - THUMB_HELD / 2.))
                                        .ml(px(-THUMB_HELD / 2.))
                                        .bg(theme::acc())
                                })
                            })
                    }),
            )
            .child(
                canvas(
                    move |bounds_now, _, _| bounds.set(Some(bounds_now)),
                    move |bounds, (), window, _| {
                        if !answers {
                            return;
                        }
                        // A press a little above or below the line takes it too.
                        // Its ends stay exact: a press on an icon beside it is the icon's.
                        let reach = Bounds::new(
                            gpui::point(bounds.origin.x, bounds.origin.y - px(GRAB)),
                            gpui::size(bounds.size.width, bounds.size.height + px(2. * GRAB)),
                        );
                        let down = this.clone();
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                            if phase.bubble()
                                && event.button == MouseButton::Left
                                && reach.contains(&event.position)
                            {
                                let _ = down.update(cx, |speaker, cx| {
                                    if let Some(at) = speaker.fraction_at(event.position.x) {
                                        speaker.holding = true;
                                        speaker.want(at, cx);
                                    }
                                });
                            }
                        });
                        if holding {
                            let moved = this.clone();
                            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                                if phase.bubble() {
                                    let _ = moved.update(cx, |speaker, cx| {
                                        if let Some(at) = speaker.fraction_at(event.position.x) {
                                            speaker.want(at, cx);
                                        }
                                    });
                                }
                            });
                            let up = this.clone();
                            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                                if phase.bubble() && event.button == MouseButton::Left {
                                    let _ = up.update(cx, |speaker, cx| {
                                        if let Some(at) = speaker.fraction_at(event.position.x)
                                            && speaker.held.is_some()
                                        {
                                            speaker.held = Some(at);
                                        }
                                        speaker.let_go(cx);
                                    });
                                }
                            });
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
    }

    /// The picker: a card in Up next's family, rising from the widget.
    fn picker(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let places = self.places.read(cx);
        let picker = places.picker.as_ref()?;
        let chosen = places.chosen_id(cx);
        let clock = crate::away::clock();
        let rows: Vec<_> = picker
            .rows
            .iter()
            .filter_map(|id| places.list.iter().find(|row| row.player_id == *id))
            .map(|row| {
                let id = row.player_id;
                let name = if row.is_local {
                    crate::platform::THIS_COMPUTER.to_owned()
                } else {
                    row.name.clone()
                };
                let playing = plays(row);
                let at = picker.at == Some(id);
                let choosable = row.reachable;
                div()
                    .id(SharedString::from(format!("place-{id}")))
                    .h(px(46.))
                    .px(px(10.))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .when(at, |row| row.bg(theme::fill()))
                    .when(!choosable, |row| row.opacity(0.45))
                    .when(choosable, |row| {
                        row.hover(theme::hovered)
                            .active(theme::pressed)
                            .on_hover(cx.listener(move |speaker, over: &bool, _, cx| {
                                if *over {
                                    speaker.places.update(cx, |places, cx| {
                                        if let Some(picker) = &mut places.picker {
                                            picker.at = Some(id);
                                            cx.notify();
                                        }
                                    });
                                }
                            }))
                            .on_click(cx.listener(move |speaker, _, _, cx| {
                                speaker
                                    .places
                                    .update(cx, |places, cx| places.choose(id, cx));
                            }))
                    })
                    .child(icon("speaker", 16., theme::dim()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.5))
                                    .line_height(px(17.))
                                    .font_weight(FontWeight(500.))
                                    .text_color(theme::ink())
                                    .child(name),
                            )
                            .child({
                                // A place in trouble says so in its colour,
                                // after its mark.
                                let trouble = crate::places::trouble(row, None);
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .line_height(px(15.))
                                    .text_color(
                                        trouble.as_ref().map_or(theme::dim(), |trouble| {
                                            trouble.status().text()
                                        }),
                                    )
                                    .children(
                                        trouble
                                            .as_ref()
                                            .map(|trouble| trouble.mark().draw(PICKER_MARK)),
                                    )
                                    .child(div().min_w_0().truncate().child(line(row, &clock)))
                            }),
                    )
                    .when(playing, |row| {
                        row.child(
                            crate::marks::Equalizer::for_picker(self.picker_opened, cx)
                                .draw(theme::acc()),
                        )
                    })
                    .when(chosen == Some(id), |row| {
                        row.child(icon("check", 14., theme::acc()))
                    })
            })
            .collect();
        let widget = Rc::clone(&self.icon_bounds);
        let closes = self.places.downgrade();
        let tallest = (f32::from(window.viewport_size().height)
            - crate::wall::TITLE_BAR
            - crate::wall::PLAYER_BAR
            - 12.)
            .max(4. * 48.);
        Some(
            gpui::deferred(
                div()
                    .id("picker")
                    // What is under the card takes no pointer while it is open.
                    .occlude()
                    .absolute()
                    .bottom(px(HEIGHT + 8.))
                    .right_0()
                    .w(px(PICKER))
                    .p(px(6.))
                    .max_h(px(tallest))
                    .rounded(px(12.))
                    .bg(theme::raised())
                    .shadow(crate::search::shadow())
                    .flex()
                    .flex_col()
                    // A click outside closes it; one on the speaker icon is
                    // the icon's own, which toggles it.
                    .on_mouse_down_out(move |event, _, cx| {
                        if widget
                            .get()
                            .is_some_and(|bounds| bounds.contains(&event.position))
                        {
                            return;
                        }
                        let _ = closes.update(cx, |places, cx| places.close_picker(cx));
                    })
                    // A long list scrolls within the window's height.
                    .child(
                        div()
                            .id("picker-rows")
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(rows),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

impl Render for Speaker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let places = self.places.read(cx);
        let look = places.look(cx);
        let open = places.picker.is_some();
        let moving = places.moving.is_some();
        let generation = self.player.read(cx).generation();
        // The row the keys are on is kept in sight.
        let at = places.picker.as_ref().and_then(|picker| {
            let at = picker.at?;
            Some((at, picker.rows.iter().position(|row| *row == at)?))
        });
        if at.map(|(id, _)| id) != self.scrolled_to {
            self.scrolled_to = at.map(|(id, _)| id);
            if let Some((_, index)) = at {
                self.scroll.scroll_to_item(index);
            }
        }
        match (open, self.picker_opened) {
            (true, None) => self.picker_opened = Some(Instant::now()),
            (false, Some(_)) => self.picker_opened = None,
            _ => {}
        }
        let now = Instant::now();
        let still = crate::platform::reduce_motion();
        let (lit, warn) = match &look {
            Look::There { name, trouble, .. } => {
                self.lit_name = Some(name.clone().into());
                if let Some(trouble) = trouble {
                    self.trouble = trouble.status();
                }
                (true, trouble.is_some())
            }
            _ => (false, false),
        };
        self.light.follow(lit, warn, now);
        let shown = self.light.shown(now, still);
        if shown.moving {
            window.request_animation_frame();
        }
        if !lit && shown.tint <= 0. {
            self.lit_name = None;
        }
        // The trouble's text form, since the speaker icon is drawn in it.
        let color = theme::mix(theme::acc(), self.trouble.text(), shown.warn);
        // The tint's colour over the bar, for the name's fade to end in.
        let ground = theme::mix(theme::bar_bg(), color, theme::LIT_TINT);
        let (icon_shown, slider, answers, trouble, name) = match &look {
            Look::Empty => (false, None, false, None, None),
            Look::Here { icon, slider } => (*icon, *slider, true, None, None),
            Look::There {
                slider, trouble, ..
            } => (
                true,
                *slider,
                trouble.is_none(),
                trouble.clone(),
                self.lit_name.clone(),
            ),
        };
        // In trouble the slider dims. While the music moves, and until the
        // place it went to reports, it keeps its place but does not answer.
        let dimmed = trouble.is_some();
        let player = self.player.read(cx);
        // A place taken over is not read yet: the last level holds the
        // slider's place meanwhile, so the name does not drop and rise.
        let unread = lit
            && slider.is_none()
            && player.state.is_none()
            && crate::remember::looks(cx).volume != crate::places::VolumeControl::Never;
        let slider = slider.or(self.last_level.filter(|_| unread));
        let answers = answers && !moving && !unread && !player.stale();
        self.last_level = slider.or(self.last_level);
        // A level held through a change of place or session, or once the
        // slider stops answering, is let go unsent.
        if self.gesture.is_some()
            && (self.gesture != Some(generation) || slider.is_none() || !answers)
        {
            self.cancel(cx);
        }
        let slider = slider.map(|reported| self.held.unwrap_or(reported));
        let name = name.or_else(|| self.lit_name.clone().filter(|_| shown.tint > 0.));

        let toggles = cx.listener(|speaker, _, _, cx| {
            speaker
                .places
                .update(cx, |places, cx| places.toggle_picker(cx));
        });
        let icon_color = if shown.tint > 0. {
            theme::mix(theme::dim(), color, shown.tint)
        } else {
            theme::dim()
        };
        let icon_bounds = Rc::clone(&self.icon_bounds);
        // Only the speaker icon opens the picker.
        let speaker_button = div()
            .id("speaker-icon")
            .group("speaker-icon")
            .relative()
            .size(px(ICON))
            .flex_none()
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center()
            // Open, it is lit as Up next's button is while its card is;
            // inside the lit widget, whose tint it already wears, it takes
            // the ground a control on a ground takes instead.
            .when(open, |button| {
                if shown.tint > 0. {
                    button.bg(theme::fill2())
                } else {
                    button.bg(gpui::Rgba {
                        a: 0.16,
                        ..theme::acc()
                    })
                }
            })
            .active(theme::pressed)
            .on_click(toggles)
            .child(
                icon("speaker", 15., if open { theme::acc() } else { icon_color })
                    .group_hover("speaker-icon", theme::hovered_ink),
            )
            .child(
                canvas(
                    move |bounds, _, _| icon_bounds.set(Some(bounds)),
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            );

        // The volume icon shows the level, and a click on it mutes or unmutes.
        let mute = |volume: f64, side: f32, cx: &mut Context<Self>| {
            div()
                .id("mute")
                .group("mute")
                .flex_none()
                .flex()
                .items_center()
                .when(dimmed, |button| button.opacity(0.4))
                .child(
                    icon(
                        match waves(volume) {
                            0 => "volume0",
                            1 => "volume1",
                            _ => "volume2",
                        },
                        side,
                        theme::dim(),
                    )
                    .when(answers, |icon| icon.group_hover("mute", theme::hovered_ink)),
                )
                .when(answers, |button| {
                    button
                        .active(theme::pressed)
                        .on_click(cx.listener(|speaker, _, _, cx| speaker.toggle_mute(cx)))
                })
        };

        // Hovering the name shows it whole when it does not fit, and the
        // whole of any trouble; never while the slider is held or the
        // picker is open.
        let overflows = name
            .as_ref()
            .is_some_and(|name| name_width(name, window) > NAME_ROOM + 0.5);
        let clock = crate::away::clock();
        let hover = name
            .as_ref()
            .filter(|_| (overflows || trouble.is_some()) && !self.holding && !open)
            .map(|name| {
                (
                    name.clone(),
                    trouble.as_ref().map(|trouble| {
                        (
                            SharedString::from(trouble.sentence(&clock)),
                            trouble.status(),
                        )
                    }),
                )
            });
        let body = match (&name, slider) {
            // Lit: the name, faded at its end, above the volume icon and the slider.
            (Some(name), slider) => div()
                .id("place-name")
                .flex_1()
                .min_w_0()
                .pl(px(10.))
                .pr(px(2.))
                .flex()
                .flex_col()
                .justify_center()
                .child(
                    div()
                        .id("place-name-text")
                        .relative()
                        .flex()
                        // As wide as the name, up to the room there is, so
                        // only the name shows the hover card.
                        .self_start()
                        .max_w_full()
                        .overflow_hidden()
                        .text_size(px(12.))
                        .line_height(px(14.))
                        .font_weight(FontWeight(500.))
                        .text_color(theme::ink())
                        .opacity(shown.name)
                        .top(px(shown.drop))
                        .child(div().flex_none().whitespace_nowrap().child(name.clone()))
                        // A name that does not fit fades out at its end; one
                        // that fits is left whole.
                        .when(overflows, |text| {
                            text.child(div().absolute().top_0().right_0().h_full().w(px(18.)).bg(
                                linear_gradient(
                                    90.,
                                    linear_color_stop(gpui::Rgba { a: 0., ..ground }, 0.),
                                    linear_color_stop(ground, 1.),
                                ),
                            ))
                        })
                        .when_some(hover, |text, (name, more)| {
                            text.tooltip(move |_, cx| {
                                let (name, more) = (name.clone(), more.clone());
                                cx.new(|_| HoverCard { name, more }).into()
                            })
                        }),
                )
                .children(slider.map(|volume| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .child(mute(volume, 12., cx))
                        .child(self.slider(volume, answers, dimmed, cx))
                })),
            // Here, under `always`: the volume icon, then the slider.
            (None, Some(volume)) => div()
                .id("place-name")
                .flex_1()
                .min_w_0()
                .pl(px(8.))
                .pr(px(if icon_shown { 2. } else { 8. }))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(mute(volume, 15., cx))
                .child(self.slider(volume, answers, dimmed, cx)),
            (None, None) => div().id("place-name").flex_1(),
        };

        let this = cx.entity().downgrade();
        let rests = this.clone();
        let leaves = this.clone();
        let picker = if open { self.picker(window, cx) } else { None };
        div()
            .id("speaker")
            .relative()
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .flex_none()
            .on_mouse_move(move |_, _, cx| {
                let _ = rests.update(cx, |speaker, _| speaker.gate.moved(Instant::now()));
            })
            .on_hover(move |over, _, cx| {
                if !*over {
                    let _ = leaves.update(cx, |speaker, _| speaker.gate.left());
                }
            })
            .on_scroll(cx.listener(move |speaker, event: &ScrollEvent, _, cx| {
                if slider.is_some() {
                    speaker.scroll(event, answers, cx);
                    cx.stop_propagation();
                }
            }))
            // The tint grows leftwards out of the icon's own button.
            .when(shown.tint > 0., |widget| {
                widget.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .h_full()
                        .w(gpui::relative(shown.reach))
                        .rounded(px(9.))
                        .bg(gpui::Rgba {
                            a: theme::LIT_TINT * shown.tint,
                            ..color
                        }),
                )
            })
            .child(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .items_center()
                    .child(body)
                    .when(icon_shown, |row| row.child(speaker_button)),
            )
            .children(picker)
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test fails by saying what it found"
)]
mod tests {
    use super::*;

    fn at(ms: u64, from: Instant) -> Instant {
        from + Duration::from_millis(ms)
    }

    #[test]
    fn a_dragged_level_is_sent_at_most_every_tenth_of_a_second() {
        let t0 = Instant::now();
        let mut throttle = Throttle::default();
        assert_eq!(throttle.wait(t0), None);
        throttle.sent(t0);
        assert_eq!(throttle.wait(at(30, t0)), Some(Duration::from_millis(70)));
        assert_eq!(throttle.wait(at(99, t0)), Some(Duration::from_millis(1)));
        assert_eq!(throttle.wait(at(100, t0)), None);
        throttle.sent(at(100, t0));
        assert!(throttle.wait(at(150, t0)).is_some());
    }

    #[test]
    fn a_scroll_moves_the_volume_only_after_the_pointer_rests_a_quarter_second() {
        let t0 = Instant::now();
        let mut gate = ScrollGate::default();
        gate.moved(t0);
        // Too soon: the pointer has only just come.
        assert!(!gate.scroll(TouchPhase::Started, false, at(100, t0)));
        assert!(!gate.scroll(TouchPhase::Moved, false, at(110, t0)));
        assert!(!gate.scroll(TouchPhase::Ended, false, at(120, t0)));
        // At rest long enough: the whole gesture moves it.
        assert!(gate.scroll(TouchPhase::Started, false, at(600, t0)));
        assert!(gate.scroll(TouchPhase::Moved, false, at(610, t0)));
        assert!(gate.scroll(TouchPhase::Moved, false, at(700, t0)));
        assert!(gate.scroll(TouchPhase::Ended, false, at(710, t0)));
    }

    #[test]
    fn momentum_never_moves_the_volume() {
        let t0 = Instant::now();
        let mut gate = ScrollGate::default();
        gate.moved(t0);
        assert!(gate.scroll(TouchPhase::Started, false, at(300, t0)));
        assert!(gate.scroll(TouchPhase::Ended, false, at(320, t0)));
        // What follows the fingers lifting, close behind, is momentum.
        assert!(!gate.scroll(TouchPhase::Moved, false, at(330, t0)));
        assert!(!gate.scroll(TouchPhase::Moved, false, at(400, t0)));
        // And so is what the system says is, even with no gesture seen.
        let mut gate = ScrollGate::default();
        gate.moved(t0);
        assert!(!gate.scroll(TouchPhase::Moved, true, at(900, t0)));
    }

    #[test]
    fn a_fling_that_drifts_onto_the_widget_does_nothing_and_a_new_wheel_does() {
        let t0 = Instant::now();
        let mut gate = ScrollGate::default();
        // A wheel's events, with no phases, as the pointer arrives.
        gate.moved(t0);
        assert!(!gate.scroll(TouchPhase::Moved, false, at(20, t0)));
        // The same burst, still going after the rest has passed, stays refused.
        for ms in [120, 220, 320] {
            assert!(!gate.scroll(TouchPhase::Moved, false, at(ms, t0)));
        }
        // A pause ends it; a new turn of the wheel, at rest, moves it.
        assert!(gate.scroll(TouchPhase::Moved, false, at(800, t0)));
        assert!(gate.scroll(TouchPhase::Moved, false, at(850, t0)));
        // Leaving forgets the rest.
        gate.left();
        assert!(!gate.scroll(TouchPhase::Moved, false, at(2000, t0)));
    }

    #[test]
    fn a_notch_is_a_twentieth_and_up_is_louder() {
        let lines = scrolled(ScrollDelta::Lines(gpui::point(0., 1.)));
        assert!((lines - 0.05).abs() < 1e-9);
        let pixels = scrolled(ScrollDelta::Pixels(gpui::point(px(0.), px(-100.))));
        assert!((pixels + 0.5).abs() < 1e-9);
    }

    #[test]
    fn lighting_up_grows_from_the_icon_and_the_name_follows() {
        let t0 = Instant::now();
        let mut light = Light::default();
        // As it first shows, it is out, and still.
        light.follow(false, false, t0);
        assert_eq!(light.shown(t0, false).tint, 0.);
        light.follow(true, false, t0);
        let start = light.shown(t0, false);
        assert!((start.reach - ICON / WIDTH).abs() < 1e-6);
        assert!(start.name.abs() < 1e-6);
        assert!(start.moving);
        // The name waits 60 ms, then rises 3 points as it comes.
        let early = light.shown(at(50, t0), false);
        assert!(early.name.abs() < 1e-6);
        assert!((early.drop - NAME_RISE).abs() < 1e-6);
        let done = light.shown(at(240, t0), false);
        assert!((done.reach - 1.).abs() < 1e-6 && (done.name - 1.).abs() < 1e-6);
        assert!(!light.shown(at(300, t0), false).moving);
        // Going out takes 160 ms, the name first.
        light.follow(false, false, at(1000, t0));
        let going = light.shown(at(1080, t0), false);
        assert_eq!(going.name, 0.);
        assert!(going.tint > 0.);
        assert_eq!(light.shown(at(1160, t0), false).tint, 0.);
    }

    #[test]
    fn with_reduce_motion_the_light_only_fades() {
        let t0 = Instant::now();
        let mut light = Light::default();
        light.follow(false, false, t0);
        light.follow(true, false, t0);
        let shown = light.shown(at(100, t0), true);
        assert_eq!(shown.reach, 1.);
        assert_eq!(shown.drop, 0.);
        assert!(shown.tint > 0. && shown.tint < 1.);
    }

    use gpui::{TestAppContext, VisualTestContext};
    use ritornel_contract::{Event, PlaybackEvent};

    use crate::link::LinkEvent;
    use crate::places::tests::{
        Answers, Asked, LIVING_ROOM, Rig, connect, hub, listed, place, places, rig, state, target,
        until,
    };

    /// The widget in a window, on a hub with places, and what it was asked.
    fn widget(cx: &mut TestAppContext) -> (Rig, Entity<Speaker>, Asked, &mut VisualTestContext) {
        let rig = rig(cx, None);
        let (net, asked) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        let (places, player) = (rig.places.clone(), rig.player.clone());
        let (speaker, cx) =
            cx.add_window_view(move |_, cx| Speaker::new(places.clone(), player.clone(), cx));
        (rig, speaker, asked, cx)
    }

    fn sent(asked: &Asked) -> Vec<f64> {
        asked
            .of("playback.set")
            .iter()
            .filter_map(|params| params["volume"].as_f64())
            .collect()
    }

    #[gpui::test]
    fn a_drag_sends_ten_times_a_second_once_more_when_let_go_and_holds_its_level(
        cx: &mut TestAppContext,
    ) {
        let (rig, speaker, asked, cx) = widget(cx);
        // Under `always`, this computer's own slider shows.
        cx.update(|_, cx| {
            crate::remember::set_volume_control(crate::places::VolumeControl::Always, cx);
        });
        let settle = |cx: &mut VisualTestContext, count: usize| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while sent(&asked).len() < count {
                assert!(Instant::now() < deadline, "the level was never sent");
                cx.run_until_parked();
                std::thread::yield_now();
            }
        };
        speaker.update(cx, |speaker, cx| {
            speaker.holding = true;
            speaker.want(0.3, cx);
            speaker.want(0.32, cx);
            speaker.want(0.35, cx);
        });
        settle(cx, 1);
        cx.run_until_parked();
        assert_eq!(sent(&asked), [0.3]);
        // The place says another level meanwhile: the slider keeps its own.
        let mut report = state(0.9);
        report.revision = 40;
        rig.link.update(cx, |_, cx| {
            cx.emit(LinkEvent::Server(Event::Playback(Box::new(
                PlaybackEvent {
                    player: None,
                    state: report,
                },
            ))));
        });
        assert_eq!(speaker.read_with(cx, |speaker, _| speaker.held), Some(0.35));
        // A tenth of a second on, the last level goes.
        cx.executor().advance_clock(SEND_EVERY);
        settle(cx, 2);
        assert_eq!(sent(&asked), [0.3, 0.35]);
        // Let go, it is sent once more, and stands over a late report of
        // a level sent before it.
        speaker.update(cx, |speaker, cx| speaker.let_go(cx));
        settle(cx, 3);
        assert_eq!(sent(&asked), [0.3, 0.35, 0.35]);
        let mut late = state(0.3);
        late.revision = 41;
        rig.link.update(cx, |_, cx| {
            cx.emit(LinkEvent::Server(Event::Playback(Box::new(
                PlaybackEvent {
                    player: None,
                    state: late,
                },
            ))));
        });
        let shown = rig.player.read_with(cx, |player, _| {
            player.state.as_ref().map(|state| state.volume)
        });
        assert_eq!(shown, Some(0.35));
        assert_eq!(speaker.read_with(cx, |speaker, _| speaker.held), None);
    }

    #[gpui::test]
    fn a_press_just_above_the_slider_takes_it_and_one_past_its_ends_does_not(
        cx: &mut TestAppContext,
    ) {
        let (_rig, speaker, _, cx) = widget(cx);
        cx.update(|_, cx| {
            crate::remember::set_volume_control(crate::places::VolumeControl::Always, cx);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let Some(slider) = speaker.read_with(cx, |speaker, _| speaker.slider.get()) else {
            panic!("the slider was not drawn");
        };
        let middle = slider.center().y;
        let press = |at: gpui::Point<Pixels>, cx: &mut VisualTestContext| {
            cx.simulate_mouse_down(at, MouseButton::Left, gpui::Modifiers::default());
            let held = speaker.read_with(cx, |speaker, _| speaker.holding);
            cx.simulate_mouse_up(at, MouseButton::Left, gpui::Modifiers::default());
            held
        };
        // Past either end, though level with it: the icons beside it are there.
        assert!(!press(gpui::point(slider.left() - px(2.), middle), cx));
        assert!(!press(gpui::point(slider.right() + px(2.), middle), cx));
        // A little above or below it.
        let across = slider.center().x;
        assert!(press(gpui::point(across, slider.top() - px(GRAB - 1.)), cx));
        assert!(press(
            gpui::point(across, slider.bottom() + px(GRAB - 1.)),
            cx
        ));
        assert!(!press(
            gpui::point(across, slider.top() - px(GRAB + 2.)),
            cx
        ));
    }

    #[gpui::test]
    fn a_click_on_the_volume_icon_ends_a_scroll_before_it_mutes(cx: &mut TestAppContext) {
        let (_rig, speaker, asked, cx) = widget(cx);
        cx.update(|_, cx| {
            crate::remember::set_volume_control(crate::places::VolumeControl::Always, cx);
        });
        // A scroll: one level sent, the next waiting on the throttle.
        speaker.update(cx, |speaker, cx| {
            speaker.want(0.3, cx);
            speaker.want(0.35, cx);
            speaker.toggle_mute(cx);
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        while asked.of("playback.set").len() < 3 {
            assert!(Instant::now() < deadline, "the sets were never sent");
            cx.run_until_parked();
            std::thread::yield_now();
        }
        // Nothing the scroll held is sent after the mute.
        cx.executor().advance_clock(GESTURE_GAP);
        cx.run_until_parked();
        let sets = asked.of("playback.set");
        assert_eq!(sets.len(), 3);
        assert_eq!(sets[1]["volume"].as_f64(), Some(0.35));
        assert_eq!(sets[2]["muted"], serde_json::json!(true));
        assert!(sets[2].get("volume").is_none());
    }

    #[gpui::test]
    fn the_widget_lights_up_for_another_place_and_warns_when_it_is_out_of_reach(
        cx: &mut TestAppContext,
    ) {
        let (rig, speaker, _, cx) = widget(cx);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(!speaker.read_with(cx, |speaker, _| speaker.light.lit));
        rig.places
            .update(cx, |places, cx| places.choose(LIVING_ROOM, cx));
        until(cx, "followed the living room", |cx| {
            target(&rig, cx) == Some(LIVING_ROOM)
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let (lit, warn, name) = speaker.read_with(cx, |speaker, _| {
            (
                speaker.light.lit,
                speaker.light.warn,
                speaker.lit_name.clone(),
            )
        });
        assert!(lit && !warn);
        assert_eq!(name.as_deref(), Some("Living Room"));
        // The living room goes out of reach: the warning, and the slider
        // stops answering.
        let list: Vec<_> = places()
            .into_iter()
            .map(|row| {
                if row.player_id == LIVING_ROOM {
                    place(LIVING_ROOM, "Living Room", false, false)
                } else {
                    row
                }
            })
            .collect();
        rig.places.update(cx, |places, cx| places.took(list, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(speaker.read_with(cx, |speaker, _| speaker.light.warn));
        let adjustable = rig
            .places
            .read_with(cx, |places, cx| places.look(cx).adjustable());
        assert!(!adjustable);
        assert!(rig.player.read_with(cx, |player, _| player.out_of_reach));
        // Back here, it goes out.
        rig.places.update(cx, |places, cx| {
            places.choose(crate::places::tests::HERE, cx)
        });
        until(cx, "came back here", |cx| target(&rig, cx).is_none());
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(!speaker.read_with(cx, |speaker, _| speaker.light.lit));
    }

    #[test]
    fn trouble_turns_the_colour_over_300_ms_and_nothing_moves() {
        let t0 = Instant::now();
        let mut light = Light::default();
        light.follow(true, false, t0);
        light.follow(true, true, at(1000, t0));
        let halfway = light.shown(at(1150, t0), false);
        assert!((halfway.warn - 0.5).abs() < 1e-3);
        assert!((halfway.reach - 1.).abs() < 1e-6);
        assert_eq!(light.shown(at(1300, t0), false).warn, 1.);
    }
}
