//! A record on a page: its cover, its title on up to two lines, and a quiet
//! line under it. A click opens its page. A record with no cover gets soft
//! colour from its name, as a tile does.
//!
//! Also your years with an artist or an album as one picture: one bar a year
//! across your whole history, as tall as you listened, in one hue borrowed
//! from the artwork, with the year you played most in full. It is a picture,
//! not a chart: no counts, and nothing changes under the pointer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Bounds, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement,
    Pixels, RenderImage, SharedString, Stateful, StatefulInteractiveElement, Styled, StyledImage,
    Window, canvas, div, img, prelude::FluentBuilder, px,
};
use ritornel_contract::{ArtColor, Id, YearPlays, YearSpan};

use crate::theme;
use crate::tracked::tracked;
use crate::words::{axis, bars, tint};

/// How a record's words are set: the first shelf's are a little larger.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Words {
    pub title: f32,
    pub sub: f32,
    pub weight: f32,
}

/// Where a page's record covers are, by their record, as last drawn: a
/// cover clicked travels from there to its page, and back to it on Back.
/// The one travelling home is hidden meanwhile, so that it lands rather
/// than doubles.
///
/// A record may show twice on a page: on an artist's page, as its cover on
/// its shelf and small among the records played most. Only the first is
/// where a cover lands; the second is only where one takes off from, when
/// it is the one clicked.
#[derive(Default)]
pub(crate) struct Landings {
    at: RefCell<HashMap<Id, Bounds<Pixels>>>,
    elsewhere: RefCell<HashMap<Id, Bounds<Pixels>>>,
    pub away: Cell<Option<Id>>,
}

impl Landings {
    /// Where a record's cover was last drawn, in the window.
    pub(crate) fn bounds(&self, record: Id) -> Option<Bounds<Pixels>> {
        self.at.borrow().get(&record).copied()
    }

    /// Takes where a cover is drawn, as it is drawn: where it lands.
    pub(crate) fn mark(self: &Rc<Self>, record: Id) -> impl IntoElement + use<> {
        let landings = Rc::clone(self);
        canvas(
            move |bounds, _, _| {
                landings.at.borrow_mut().insert(record, bounds);
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full()
    }

    /// Takes where a cover that is only taken off from is drawn.
    pub(crate) fn mark_elsewhere(self: &Rc<Self>, record: Id) -> impl IntoElement + use<> {
        let landings = Rc::clone(self);
        canvas(
            move |bounds, _, _| {
                landings.elsewhere.borrow_mut().insert(record, bounds);
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full()
    }

    fn hidden(&self, record: Id) -> bool {
        self.away.get() == Some(record)
    }
}

/// A record's cover, lifted to travel to its page: where it was, its
/// picture, and the page it came from, to land back on.
pub(crate) struct Lift {
    pub from: Bounds<Pixels>,
    pub image: Option<Arc<RenderImage>>,
    pub landings: Rc<Landings>,
}

impl Lift {
    /// The cover of `record` clicked where it lands, as last drawn.
    pub(crate) fn of(
        landings: &Rc<Landings>,
        record: Id,
        image: Option<Arc<RenderImage>>,
    ) -> Option<Self> {
        Some(Self {
            from: landings.bounds(record)?,
            image,
            landings: Rc::clone(landings),
        })
    }

    /// The cover of `record` clicked somewhere it is only taken off from.
    pub(crate) fn of_elsewhere(
        landings: &Rc<Landings>,
        record: Id,
        image: Option<Arc<RenderImage>>,
    ) -> Option<Self> {
        let from = landings.elsewhere.borrow().get(&record).copied()?;
        Some(Self {
            from,
            image,
            landings: Rc::clone(landings),
        })
    }
}

/// One record as a page shows it.
pub(crate) struct Card {
    pub title: SharedString,
    /// The line under its title.
    pub sub: String,
    /// Its cover, or its soft colour when it has none; `None` while the cover comes.
    pub picture: Option<Arc<RenderImage>>,
    pub side: f32,
    pub words: Words,
    /// The page's landings, and the record they know it by.
    pub landing: Option<(Rc<Landings>, Id)>,
    /// How strongly its menu's mark shows: a ring round its cover and the
    /// menu's fill behind its words.
    pub menu: f32,
}

/// One record: its cover, its title and the line under it.
pub(crate) fn card(id: impl Into<ElementId>, card: Card, window: &Window) -> Stateful<Div> {
    let Card {
        title,
        sub,
        picture,
        side,
        words,
        landing,
        menu,
    } = card;
    let away = landing
        .as_ref()
        .is_some_and(|(landings, record)| landings.hidden(*record));
    let picture = picture.filter(|_| !away);
    let title = balanced(&title, side, words.title, words.weight, window).map_or(title, Into::into);
    let id = id.into();
    let group: SharedString = format!("record-{id}").into();
    div()
        .id(id)
        .group(group.clone())
        .w(px(side))
        .flex_none()
        .flex()
        .flex_col()
        .child(
            div()
                .id("art")
                .relative()
                .size(px(side))
                .rounded(px(10.))
                // While its cover travels, nothing of it is left here: not
                // its ground nor its hairline (the slot is hidden whole).
                .when(!away, |art| art.bg(theme::fill()))
                .group_hover(group.clone(), theme::lifted)
                .group_active(group, theme::settled)
                .when(!away, |art| {
                    art.children(crate::frost::piece(
                        crate::frost::Fill::Solid(theme::fill()),
                        1.,
                    ))
                })
                .children(picture.as_ref().and_then(|picture| {
                    crate::frost::piece(crate::frost::Fill::Picture(Arc::clone(picture)), 1.)
                }))
                .children(picture.map(|picture| {
                    img(picture)
                        .absolute()
                        .top_0()
                        .left_0()
                        .size(px(side))
                        .rounded(px(10.))
                        .object_fit(ObjectFit::Cover)
                }))
                .children(landing.map(|(landings, record)| landings.mark(record)))
                // A hairline inside the edge, so a dark cover meets a dark page cleanly.
                .when(!away, |art| {
                    art.child(
                        div()
                            .absolute()
                            .size_full()
                            .rounded(px(10.))
                            .border_1()
                            .border_color(gpui::rgba(0x80808024)),
                    )
                })
                .when(menu > 0. && !away, |art| {
                    art.child(crate::menu::ring(side, 10., menu))
                }),
        )
        .child(
            div()
                .mt(px(9.))
                .when(menu > 0., |words| {
                    let fill = crate::menu::fill(menu);
                    words.rounded(px(4.)).bg(fill).shadow(vec![
                        gpui::BoxShadow::new(px(0.), px(0.), gpui::Hsla::from(fill))
                            .spread_radius(px(4.)),
                    ])
                })
                .child(
                    div()
                        .w_full()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(words.title))
                        .line_height(gpui::relative(1.3))
                        .font_weight(FontWeight(words.weight))
                        .line_clamp(2)
                        .child(title),
                )
                .when(!sub.is_empty(), |card| {
                    card.child(
                        div()
                            .truncate()
                            .text_size(px(words.sub))
                            .text_color(theme::dim())
                            .font_features(crate::bar::tabular())
                            .child(sub),
                    )
                }),
        )
}

/// A title broken already, by its text, and the bits of its width, size and weight.
type Broken = HashMap<(String, u32, u32, u32), Option<String>>;

/// Cap on cached wrapped titles so long browsing sessions stay bounded.
const MAX_BROKEN: usize = 2048;

thread_local! {
    static BROKEN: RefCell<Broken> = RefCell::new(HashMap::new());
}

fn cache_broken(cache: &mut Broken, key: (String, u32, u32, u32), value: Option<String>) {
    if cache.len() >= MAX_BROKEN && !cache.contains_key(&key) {
        cache.clear();
    }
    cache.insert(key, value);
}

/// Where a title of two lines breaks, as WebKit's `text-wrap: pretty`
/// breaks a short paragraph: at the break that makes its longer line the
/// shortest, so that the lines balance rather than leave one word alone.
/// `None` when it fits on one line, or needs more than two.
fn balanced(title: &str, width: f32, size: f32, weight: f32, window: &Window) -> Option<String> {
    let key = (
        title.to_owned(),
        width.to_bits(),
        size.to_bits(),
        weight.to_bits(),
    );
    if let Some(known) = BROKEN.with(|broken| broken.borrow().get(&key).cloned()) {
        return known;
    }
    let measure = |text: &str| {
        let mut font = window.text_style().font();
        font.weight = FontWeight(weight);
        let run = gpui::TextRun {
            len: text.len(),
            font,
            color: theme::ink().into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        f32::from(
            window
                .text_system()
                .shape_line(text.to_owned().into(), px(size), &[run], None)
                .width,
        )
    };
    let words: Vec<&str> = title.split(' ').collect();
    let broken = if words.len() < 2 || measure(title) <= width {
        None
    } else {
        (1..words.len())
            .filter_map(|at| {
                let (first, last) = (words.get(..at)?.join(" "), words.get(at..)?.join(" "));
                let longer = measure(&first).max(measure(&last));
                (longer <= width).then_some((longer, format!("{first}\n{last}")))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, broken)| broken)
    };
    BROKEN.with(|cache| cache_broken(&mut cache.borrow_mut(), key, broken.clone()));
    broken
}

/// A shelf's or a section's heading: 18 points, a little drawn in.
pub(crate) fn heading(text: impl Into<SharedString>, window: &gpui::Window) -> impl IntoElement {
    let _ = window;
    heading_sized(text, 18.)
}

/// A heading of `size` points, at 650 and drawn in by a hundredth.
pub(crate) fn heading_sized(text: impl Into<SharedString>, size: f32) -> impl IntoElement {
    tracked(
        text.into(),
        size,
        size * 1.4,
        FontWeight(650.),
        theme::ink(),
        -0.01,
    )
}

/// The picture of your years, at most 620 points across.
pub(crate) fn years(
    title: impl Into<SharedString>,
    years: &[YearPlays],
    span: YearSpan,
    color: Option<ArtColor>,
    window: &gpui::Window,
) -> Div {
    let accent = match tint(color) {
        Some((hue, saturation)) => theme::tinted(hue, saturation),
        None => theme::acc(),
    };
    let quiet = gpui::Rgba { a: 0.46, ..accent };
    let mut columns = div().h(px(132.)).flex().items_end().gap(px(3.));
    for bar in bars(years, span) {
        columns = columns.child(if bar.height == 0. {
            // A year with no plays is a hairline, so the years still read across.
            div().flex_1().h(px(2.)).rounded(px(1.)).bg(theme::fill2())
        } else {
            div().flex_1().h_full().flex().items_end().child(
                div()
                    .w_full()
                    .h(gpui::relative(bar.height / 100.))
                    .rounded_tl(px(3.))
                    .rounded_tr(px(3.))
                    .rounded_bl(px(1.))
                    .rounded_br(px(1.))
                    .bg(if bar.peak { accent } else { quiet }),
            )
        });
    }
    let [first, middle, last] = axis(span);
    div()
        .flex()
        .flex_col()
        .child(div().mb(px(14.)).child(heading(title, window)))
        .child(
            div().max_w(px(620.)).child(columns).child(
                div()
                    .mt(px(6.))
                    .flex()
                    .justify_between()
                    .text_size(px(11.5))
                    .text_color(theme::faint())
                    .font_features(crate::bar::tabular())
                    .child(first)
                    .child(middle)
                    .child(last),
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_title_cache_stays_bounded() {
        let mut cache = Broken::new();
        for i in 0..(MAX_BROKEN + 25) {
            cache_broken(&mut cache, (format!("title-{i}"), 0, 0, 0), None);
        }
        assert!(cache.len() <= MAX_BROKEN);
    }
}
