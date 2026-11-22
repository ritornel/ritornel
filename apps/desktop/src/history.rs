//! History: one picture of your listening life (`pages/History.svelte`;
//! docs/design/ui.md, "Kinds of page": a picture). A stream across all
//! your years in one quiet hue, as tall as you listened, in layers by the
//! year you first heard each artist, the oldest at the bottom and the
//! newest on top (`stream.rs`). The picture runs flush from one edge of the
//! window to the other, its first month at one and its last at the other,
//! a picture and not a framed chart; what is written on it and under it
//! keeps to the page's margins. Under it, at the left a caption, which says
//! what a layer is and who made it up that month when one is pointed at (a
//! click holds it), and at the right this day in years gone by
//! (`thisday.rs`). With less room the two stand one under the other.
//!
//! It counts every play, matched to the library or not: History is the
//! record of all of it. It reads `history.stream` and `history.onThisDay`,
//! and again once plays have settled after the library says they changed.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    ClickEvent, Context, Entity, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveElement,
    IntoElement, MouseMoveEvent, ParentElement, PathBuilder, Pixels, Point, Render, Rgba,
    ScrollEvent, StatefulInteractiveElement, Styled, StyledText, Subscription, Task, Window,
    canvas, div, point, px,
};
use ritornel_contract::methods::{HistoryOnThisDay, HistoryStreamCall};
use ritornel_contract::{Event, HistoryStream, LibraryChange, NoParams, OnThisDayParams, Phrase};

use crate::link::{Link, LinkEvent};
use crate::net::Net;
use crate::stream::{self, Drawn, Pointed};
use crate::theme;
use crate::thisday::{self, Remembered};
use crate::wall::viewport_of;
use crate::words::plural;

/// The page's measure: it centres once the window is wider than this.
const MEASURE: f32 = 2000.;
/// Half the width of the month the pointer is at, written over the
/// picture, near enough.
const WHEN_HALF: f32 = 52.;
/// How long a layer takes to come forward or stand back.
const FADE_MS: f32 = 180.;
/// How long plays are left to settle before the picture is read again.
const SETTLE: Duration = Duration::from_secs(2);
/// From where the picture starts, what stands above and below it on a
/// page as tall as the window: it takes the rest, and never less than 280.
const AROUND: f32 = 282.;
const LEAST: f32 = 280.;
/// Wide enough under the picture for the caption and this day side by side.
const TWO_COLUMNS: f32 = 1100.;

/// What the page shows, read at once.
struct Read {
    stream: HistoryStream,
    /// This day in other years, as remembered. Empty when it could not be
    /// read: the picture goes on without it.
    remembered: Vec<Remembered>,
}

/// One opacity on its way from one value to another, as a CSS transition
/// of `opacity` would take it.
#[derive(Debug, Clone, Copy)]
struct Fade {
    from: f32,
    to: f32,
    start: Instant,
}

impl Fade {
    fn at(&self, now: Instant) -> f32 {
        let t = crate::moves::ease(crate::moves::progress(
            crate::moves::since(self.start, now),
            FADE_MS,
        ));
        self.from + (self.to - self.from) * t
    }

    /// Sets it going towards `to`, from wherever it is now.
    fn aim(&mut self, to: f32, now: Instant, still: bool) {
        if self.to != to {
            self.from = if still { to } else { self.at(now) };
            self.to = to;
            self.start = now;
        }
    }

    fn moving(&self, now: Instant) -> bool {
        crate::moves::since(self.start, now) < FADE_MS && self.from != self.to
    }
}

/// A layer's band and its name, each fading.
type Fades = (Fade, Fade);

pub(crate) struct HistoryPage {
    net: Option<Arc<Net>>,
    read: Option<Read>,
    failed: bool,
    revision: u64,
    /// Whether it is behind another page, and whether plays came meanwhile.
    freshness: crate::stage::Freshness,
    reading: Option<Task<()>>,
    /// Waiting for plays to settle before reading again.
    quiet: Option<Task<()>>,
    /// The picture as last drawn, and what it was drawn for.
    drawn: Option<(DrawnFor, Rc<Drawn>)>,
    hover: Option<Pointed>,
    held: Option<Pointed>,
    fades: HashMap<i32, Fades>,
    /// Where the picture stands in the window, for the pointer.
    origin: Rc<Cell<Point<Pixels>>>,
    pub top: f32,
    elastic: crate::elastic::Elastic,
    /// How tall the page was when last drawn.
    height: Rc<Cell<f32>>,
    _link: Subscription,
}

/// What a picture was drawn for: the same again draws nothing new.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DrawnFor {
    width: f32,
    height: f32,
    inset: f32,
    revision: u64,
}

impl HistoryPage {
    pub(crate) fn new(link: &Entity<Link>, cx: &mut Context<Self>) -> Self {
        // Plays come in one at a time as you listen, and in thousands as a
        // sync runs: the picture is read again once they have settled.
        let followed = cx.subscribe(
            link,
            |page: &mut Self, _, event: &LinkEvent, cx| match event {
                LinkEvent::Connected(net) => {
                    page.net = Some(Arc::clone(net));
                    if page.freshness.heard(false).is_some() {
                        page.load(cx);
                    }
                }
                LinkEvent::Server(Event::Library(event))
                    if event.library_revision > page.revision
                        && event.changed.contains(&LibraryChange::Stats)
                        && page.freshness.heard(true).is_some() =>
                {
                    page.quiet = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SETTLE).await;
                        this.update(cx, |page, cx| page.load(cx)).ok();
                    }));
                }
                _ => {}
            },
        );
        let mut page = Self {
            net: link.read(cx).net(),
            read: None,
            failed: false,
            revision: 0,
            freshness: crate::stage::Freshness::default(),
            reading: None,
            quiet: None,
            drawn: None,
            hover: None,
            held: None,
            fades: HashMap::new(),
            origin: Rc::new(Cell::new(Point::default())),
            top: 0.,
            elastic: crate::elastic::Elastic::default(),
            height: Rc::new(Cell::new(0.)),
            _link: followed,
        };
        page.load(cx);
        page
    }

    /// The page shown again, or put behind another: plays that came while
    /// it was behind are read.
    pub(crate) fn seen(&mut self, seen: bool, cx: &mut Context<Self>) {
        if self.freshness.shown(seen).is_some() {
            self.load(cx);
        }
    }

    /// Reads the picture and this day in other years.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(net) = &self.net else { return };
        let client = Arc::clone(&net.client);
        let read = net.rt().spawn(async move {
            let (stream, day) = tokio::join!(
                client.call::<HistoryStreamCall>(&NoParams {}),
                client.call::<HistoryOnThisDay>(&OnThisDayParams { day: None }),
            );
            (stream, day.ok())
        });
        self.reading = Some(cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |page, cx| {
                match answer {
                    Ok((Ok(stream), day)) => {
                        page.revision = stream.library_revision;
                        let remembered = match (day, stream::this_year(&stream)) {
                            (Some(day), Some(year)) => thisday::remember(&day, year),
                            _ => Vec::new(),
                        };
                        page.read = Some(Read { stream, remembered });
                        page.failed = false;
                    }
                    _ => page.failed = page.read.is_none(),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Whether it is ready to be seen: read, or known not to be readable.
    pub(crate) fn ready(&self) -> bool {
        self.read.is_some() || self.failed
    }

    /// Scrolls the page by `dy` at once, as a script's jump does.
    pub(crate) fn jump(&mut self, dy: f32, viewport: f32) {
        let max = (self.height.get() - viewport).max(0.);
        self.top = (self.top + dy).clamp(0., max);
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (_, viewport) = viewport_of(window);
        let max = (self.height.get() - viewport).max(0.);
        self.elastic
            .scroll(&mut self.top, crate::elastic::Wheel::of(event), max);
        cx.notify();
    }

    /// Where the pointer is over the picture: a month, and the layer it is on.
    fn pointed(&self, at: Point<Pixels>) -> Option<Pointed> {
        let read = self.read.as_ref()?;
        let (_, drawn) = self.drawn.as_ref()?;
        let origin = self.origin.get();
        stream::pointed(
            drawn,
            read.stream.months,
            f32::from(at.x - origin.x),
            f32::from(at.y - origin.y),
        )
    }

    /// The pointer held at `at`, in the window, as a script holds it.
    pub(crate) fn point(&mut self, at: Point<Pixels>) {
        self.hover = self.pointed(at);
    }

    /// A click on the `n`th layer, the newest first, as the real app's
    /// measure mode dispatches one: with no place, so at the first month.
    pub(crate) fn hold(&mut self, n: usize) {
        self.held = self
            .drawn
            .as_ref()
            .and_then(|(_, drawn)| drawn.layers.get(n))
            .map(|layer| Pointed {
                month: 0,
                year: Some(layer.year),
            });
    }

    /// The picture for this width and height, drawn again only when they
    /// or what it shows have changed.
    fn drawn(&mut self, width: f32, height: f32, inset: f32) -> Option<Rc<Drawn>> {
        let read = self.read.as_ref()?;
        let wanted = DrawnFor {
            width,
            height,
            inset,
            revision: self.revision,
        };
        if let Some((drawn_for, drawn)) = &self.drawn
            && *drawn_for == wanted
        {
            return Some(Rc::clone(drawn));
        }
        let drawn = Rc::new(stream::draw(&read.stream, width, height, inset));
        self.drawn = Some((wanted, Rc::clone(&drawn)));
        Some(drawn)
    }

    /// The picture, `width` across and `height` high, and what is written
    /// on it. Returns whether anything on it is still moving.
    fn picture(
        &mut self,
        drawn: &Rc<Drawn>,
        inset: f32,
        cx: &mut Context<Self>,
    ) -> (gpui::Stateful<gpui::Div>, bool) {
        let now = Instant::now();
        let still = crate::platform::reduce_motion();
        let lit = self
            .held
            .and_then(|held| held.year)
            .or(self.hover.and_then(|hover| hover.year));
        // With a layer lit, the others stand back.
        let mut moving = false;
        let mut looks: Vec<(f32, f32)> = Vec::with_capacity(drawn.layers.len());
        for layer in &drawn.layers {
            let back = lit.is_some() && lit != Some(layer.year);
            let (band, text) = self.fades.entry(layer.year).or_insert_with(|| {
                let rest = |to| Fade {
                    from: to,
                    to,
                    start: now,
                };
                (rest(1.), rest(1.))
            });
            band.aim(if back { 0.45 } else { 1. }, now, still);
            text.aim(if back { 0.3 } else { 1. }, now, still);
            moving |= band.moving(now) || text.moving(now);
            looks.push((band.at(now), text.at(now)));
        }
        let bg = theme::bg();
        let acc = theme::acc();
        let ink = theme::ink();
        let hair = self.hover.map(|hover| hover.month as f32 * drawn.step);
        let origin = Rc::clone(&self.origin);
        let painted = Rc::clone(drawn);
        // Its layers' colours, as painted below, for Up next's frost.
        let frosted = crate::frost::wanted().then(|| {
            let colors: Vec<Rgba> = drawn
                .layers
                .iter()
                .zip(&looks)
                .map(|(layer, (band, _))| {
                    let fill = if lit == Some(layer.year) {
                        acc
                    } else {
                        theme::mix(bg, acc, layer.shade / 100.)
                    };
                    theme::mix(bg, fill, *band)
                })
                .collect();
            crate::frost::Fill::Stream(Rc::clone(drawn), Rc::new(colors))
        });
        let picture = canvas(
            move |bounds, window, _| {
                origin.set(bounds.origin);
                if let Some(fill) = frosted {
                    let clip = window.content_mask().bounds.intersect(&bounds);
                    crate::frost::note(bounds, clip, fill, 1.);
                }
            },
            move |bounds, (), window, _| {
                let at = bounds.origin;
                let to = |(x, y): (f32, f32)| point(at.x + px(x), at.y + px(y));
                for (layer, (band, _)) in painted.layers.iter().zip(&looks) {
                    let points = stream::outline(&layer.top, &layer.bottom, painted.step);
                    let Some((&first, rest)) = points.split_first() else {
                        continue;
                    };
                    // One hue, from old to new: the newer a layer, the more of
                    // the accent; the one lit takes all of it.
                    let fill = if lit == Some(layer.year) {
                        acc
                    } else {
                        theme::mix(bg, acc, layer.shade / 100.)
                    };
                    let fill = theme::mix(bg, fill, *band);
                    let mut shape = PathBuilder::fill();
                    // Edged in the page's colour, so that layers part.
                    let mut edge = PathBuilder::default().with_style(gpui::PathStyle::Stroke(
                        gpui::StrokeOptions {
                            line_width: 0.75,
                            line_join: gpui::LineJoin::Round,
                            ..Default::default()
                        },
                    ));
                    shape.move_to(to(first));
                    edge.move_to(to(first));
                    for &next in rest {
                        shape.line_to(to(next));
                        edge.line_to(to(next));
                    }
                    shape.close();
                    edge.close();
                    if let Ok(path) = shape.build() {
                        window.paint_path(path, Hsla::from(fill));
                    }
                    if let Ok(path) = edge.build() {
                        window.paint_path(path, Hsla::from(bg));
                    }
                }
                for (layer, (_, text)) in painted.layers.iter().zip(&looks) {
                    let Some(label) = layer.label else { continue };
                    let color = if lit == Some(layer.year) {
                        bg
                    } else {
                        Rgba {
                            a: 0.86 * text,
                            ..ink
                        }
                    };
                    crate::tracked::paint_centred(
                        layer.name.clone(),
                        label.size,
                        FontWeight(650.),
                        color,
                        -0.01,
                        f32::from(at.x) + label.x,
                        f32::from(at.y) + label.y,
                        window,
                    );
                }
                if let Some(x) = hair {
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(
                            point(at.x + px(x - 0.5), at.y),
                            gpui::size(px(1.), bounds.size.height),
                        ),
                        Rgba { a: 0.55, ..ink },
                    ));
                }
            },
        )
        .absolute()
        .size_full();
        let when = match (&self.read, self.hover) {
            (Some(read), Some(hover)) => {
                let x = stream::within(
                    hover.month as f32 * drawn.step,
                    WHEN_HALF,
                    drawn.width,
                    inset,
                );
                Some(
                    div()
                        .absolute()
                        .top(px(-4.))
                        .left(px(x - 100.))
                        .w(px(200.))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .px(px(8.))
                                .py(px(2.))
                                .rounded_full()
                                .bg(ink)
                                .text_color(bg)
                                .text_size(px(11.5))
                                .line_height(line(11.5, 1.4))
                                .font_weight(FontWeight(650.))
                                .whitespace_nowrap()
                                .child(stream::month_name(&read.stream, hover.month)),
                        ),
                )
            }
            _ => None,
        };
        let element = div()
            .id("stream")
            .relative()
            .w(px(drawn.width))
            .h(px(drawn.height))
            .child(picture)
            .children(when)
            .on_mouse_move(cx.listener(|page, event: &MouseMoveEvent, _, cx| {
                let at = page.pointed(event.position);
                if at != page.hover {
                    page.hover = at;
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|page, hovered: &bool, _, cx| {
                if !*hovered && page.hover.is_some() {
                    page.hover = None;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|page, event: &ClickEvent, _, cx| {
                let at = page.pointed(event.position());
                page.held = at.filter(|at| at.year.is_some());
                cx.notify();
            }));
        (element, moving)
    }
}

/// Words with some set apart, as the real page's `<b>` sets them: in the
/// ink and at 600. `italic` is a part of them set in italic, as its `<i>`
/// is. With `pretty`, their size and the room they have, they break across
/// lines as the real page's `text-wrap: pretty` breaks them (`pretty.rs`).
fn rich(
    phrases: &[Phrase],
    italic: Option<std::ops::Range<usize>>,
    pretty: Option<(f32, f32)>,
    window: &Window,
) -> StyledText {
    let mut text = String::new();
    let mut strong = Vec::new();
    for phrase in phrases {
        let start = text.len();
        text.push_str(&phrase.text);
        if phrase.strong {
            strong.push(start..text.len());
        }
    }
    if let Some((size, room)) = pretty {
        text = broken(&text, &strong, italic.as_ref(), size, room, window);
    }
    let mut highlights: Vec<_> = strong
        .into_iter()
        .map(|range| {
            (
                range,
                HighlightStyle {
                    color: Some(theme::ink().into()),
                    font_weight: Some(FontWeight(600.)),
                    ..HighlightStyle::default()
                },
            )
        })
        .collect();
    if let Some(range) = italic {
        highlights.push((
            range,
            HighlightStyle {
                font_style: Some(FontStyle::Italic),
                ..HighlightStyle::default()
            },
        ));
    }
    StyledText::new(text).with_highlights(highlights)
}

/// `text` with the spaces it breaks at made new lines, each word measured
/// in its own weight and style. A space and a new line are a byte each, so
/// what is set apart stays where it was.
fn broken(
    text: &str,
    strong: &[std::ops::Range<usize>],
    italic: Option<&std::ops::Range<usize>>,
    size: f32,
    room: f32,
    window: &Window,
) -> String {
    let base = window.text_style().font();
    let style = |at: usize| {
        (
            strong.iter().any(|range| range.contains(&at)),
            italic.is_some_and(|range| range.contains(&at)),
        )
    };
    let measure = |from: usize, to: usize| -> f32 {
        // A run for each stretch of one weight and style.
        let mut runs = Vec::new();
        let mut at = from;
        while at < to {
            let (bold, leaning) = style(at);
            let mut end = at + 1;
            while end < to && (!text.is_char_boundary(end) || style(end) == (bold, leaning)) {
                end += 1;
            }
            let mut font = base.clone();
            if bold {
                font.weight = FontWeight(600.);
            }
            if leaning {
                font.style = FontStyle::Italic;
            }
            runs.push(gpui::TextRun {
                len: end - at,
                font,
                color: theme::ink().into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
            at = end;
        }
        let word: gpui::SharedString = text.get(from..to).unwrap_or_default().to_owned().into();
        f32::from(
            window
                .text_system()
                .shape_line(word, px(size), &runs, None)
                .width,
        )
    };
    let mut starts = vec![0];
    starts.extend(text.match_indices(' ').map(|(at, _)| at + 1));
    let widths: Vec<f32> = starts
        .iter()
        .enumerate()
        .map(|(n, &from)| {
            let to = starts.get(n + 1).map_or(text.len(), |next| next - 1);
            measure(from, to)
        })
        .collect();
    let space = measure_space(size, window);
    let Some(lines) = crate::pretty::breaks(&widths, space, room) else {
        return text.to_owned();
    };
    let mut bytes = text.as_bytes().to_vec();
    for line in lines {
        if let Some(&start) = starts.get(line)
            && let Some(space) = bytes.get_mut(start - 1)
        {
            *space = b'\n';
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| text.to_owned())
}

/// The width of a space at `size` points.
fn measure_space(size: f32, window: &Window) -> f32 {
    let run = gpui::TextRun {
        len: 1,
        font: window.text_style().font(),
        color: theme::ink().into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    f32::from(
        window
            .text_system()
            .shape_line(" ".into(), px(size), &[run], None)
            .width,
    )
}

/// The words under the picture: at 74% of the ink.
fn quiet_ink() -> Rgba {
    Rgba {
        a: 0.74,
        ..theme::ink()
    }
}

/// This day in years gone by: a few sentences as large as the caption's,
/// under a quiet label, so that one alone is a thing said and not a list
/// left short.
fn this_day(remembered: &[Remembered], room: f32, window: &Window) -> gpui::Div {
    let mut day = div()
        .flex()
        .flex_col()
        .child(div().mt(px(3.)).mb(px(10.)).child(crate::tracked::tracked(
            "On this day",
            12.5,
            f32::from(line(12.5, 1.4)),
            FontWeight(600.),
            theme::faint(),
            0.01,
        )));
    for (at, one) in remembered.iter().enumerate() {
        let plays = thisday::plays_of(one);
        let (said, italic) = match &one.record {
            Some(record) => {
                let start = plays.len() + 1;
                (
                    format!("{plays} {record}."),
                    Some(start..start + record.len()),
                )
            }
            None => (plays, None),
        };
        let said = Phrase {
            text: said,
            strong: false,
        };
        day = day.child(
            div()
                .when(at > 0, |one| one.mt(px(16.)))
                .child(
                    div()
                        .text_size(px(17.))
                        .line_height(line(17., 1.4))
                        .text_color(quiet_ink())
                        .child(rich(
                            &thisday::say_remembered(one),
                            None,
                            Some((17., room)),
                            window,
                        )),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(13.5))
                        .line_height(line(13.5, 1.4))
                        .text_color(theme::faint())
                        .child(rich(std::slice::from_ref(&said), italic, None, window)),
                ),
        );
    }
    day
}

/// A line `ratio` times as tall as its type, as WebKit sets it: it
/// truncates a computed line height to whole points, so the real page's
/// lines of 17 points at 1.4 are 23 tall, not 23.8.
fn line(size: f32, ratio: f32) -> Pixels {
    px((size * ratio).floor())
}

fn gutter(width: f32) -> f32 {
    ((width - MEASURE) / 2.).round().max(40.)
}

impl Render for HistoryPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (window_width, viewport) = viewport_of(window);
        // With Up next moving the page over, the picture runs to the card,
        // and the page keeps to the room left of it.
        let width = window_width - crate::upnext::rail(cx);
        let gutter = gutter(width);
        let content = width - 2. * gutter;
        let max = (self.height.get() - viewport).max(0.);
        self.top = self.top.clamp(0., max);
        let mut animating = self.elastic.step(Instant::now());
        let top = self.top + self.elastic.shown();

        let years = self
            .read
            .as_ref()
            .map_or(0, |read| stream::years_of(&read.stream));
        let heading = div()
            .px(px(gutter))
            .mb(px(22.))
            .flex()
            .items_baseline()
            .gap(px(14.))
            .h(px(42.))
            .child(crate::tracked::tracked(
                "History",
                30.,
                42.,
                FontWeight::BOLD,
                theme::ink(),
                -0.022,
            ))
            .when(years > 0, |heading| {
                // The tracked name has no text baseline for the row to align
                // by, so what stands beside it is set on the name's by hand.
                heading.child(
                    div()
                        .relative()
                        .top(px(-4.))
                        .text_size(px(14.))
                        .text_color(theme::dim())
                        .child(format!(
                            "{}, and what they were made of",
                            plural(years, "year")
                        )),
                )
            });

        let mut body = div().w(px(width)).flex().flex_col().child(heading);
        let empty = self
            .read
            .as_ref()
            .is_some_and(|read| read.stream.months == 0);
        if self.failed {
            body = body.child(
                div()
                    .px(px(gutter))
                    .my(px(8.))
                    .text_size(px(13.5))
                    .line_height(line(13.5, 1.4))
                    .text_color(theme::faint())
                    .child("Your history could not be read."),
            );
        } else if empty {
            body = body.child(
                crate::blank::Blank::new(
                    "Nothing yet.",
                    "As you listen, this becomes a picture of it: the years left to right, and what they were made of.",
                )
                .render(viewport),
            );
        } else if self.read.is_some() {
            // The picture is as tall as the window leaves it.
            let inset = gutter;
            let tall = LEAST.max(viewport - AROUND);
            if let Some(drawn) = self.drawn(width, tall, inset) {
                let (picture, moving) = self.picture(&drawn, inset, cx);
                animating |= moving;
                // The years under it, each under its middle.
                let mut marks = div().relative().mt(px(6.)).h(px(26.)).w(px(width));
                for mark in &drawn.years {
                    marks = marks.child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .left(px(mark.x - 60.))
                            .w(px(120.))
                            .py(px(2.))
                            .flex()
                            .justify_center()
                            .text_size(px(12.5))
                            .line_height(line(12.5, 1.4))
                            .text_color(theme::faint())
                            .font_features(crate::bar::tabular())
                            .child(mark.year.to_string()),
                    );
                }
                body = body.child(picture).child(marks);
            }
            // Under the stream, on the page's margins: what the picture says
            // at the left, and this day in years gone by at the right; one
            // under the other when the window is narrow.
            let read = self.read.as_ref();
            let said = read.map_or_else(Vec::new, |read| {
                stream::say(&read.stream, self.held.or(self.hover))
            });
            // Room for two lines, what it says of most layers pointed at, so
            // that what stands under it seldom moves as the pointer does.
            let two = content >= TWO_COLUMNS;
            let column = (content - 64.) / 2.;
            let room = if two { column } else { content };
            let say = div().min_h(px(54.)).child(
                div()
                    .max_w(px(680.))
                    .text_size(px(18.))
                    .line_height(line(18., 1.45))
                    .text_color(quiet_ink())
                    .child(rich(&said, None, Some((18., room.min(680.))), window)),
            );
            let day = read
                .filter(|read| !read.remembered.is_empty())
                .map(|read| this_day(&read.remembered, room, window));
            let cols = if two {
                div()
                    .flex()
                    .items_start()
                    .gap(px(64.))
                    .child(say.w(px(column)))
                    .children(day.map(|day| day.w(px(column))))
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(30.))
                    .child(say)
                    .children(day)
            };
            body = body.child(div().mt(px(16.)).px(px(gutter)).child(cols));
        }

        // The page measures itself: from a mark at its head to one at its foot.
        let height = Rc::clone(&self.height);
        let head = Rc::new(Cell::new(0f32));
        let start = Rc::clone(&head);
        let page = div()
            .absolute()
            .left_0()
            .top(px(-top))
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
            .child(div().h(px(20.)).flex_none())
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
        if animating {
            window.request_animation_frame();
        }
        div()
            .id("history")
            .relative()
            .size_full()
            .overflow_hidden()
            .on_scroll(cx.listener(Self::on_scroll))
            .child(page)
    }
}
