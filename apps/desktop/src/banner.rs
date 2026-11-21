//! The banner an artist's page and an album's open on (`parts/Banner.svelte`):
//! one banner, the same to the point on both, from edge to edge under the
//! title bar, made of the page's picture blurred, with the way back, the
//! name set large, the facts, Play, Shuffle and one sentence. As the page
//! scrolls it slides under the title bar until a strip of 52 points is
//! left, with the way back, the face, the name and Play; the large words
//! fade out and the strip in as it goes, at the real banner's points.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, BoxShadow, Context, FontWeight, Hsla, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, RenderImage, Rgba, SharedString, StatefulInteractiveElement, Styled,
    StyledImage, Window, div, img, linear_color_stop, linear_gradient, prelude::FluentBuilder, px,
    rgba,
};
use image::RgbaImage;
use ritornel_contract::Phrase;

use crate::frost;
use crate::icons::icon;
use crate::theme;
use crate::tracked::tracked;

/// The banner's height, and the strip that is left of it.
pub(crate) const HEIGHT: f32 = 214.;
pub(crate) const STRIP: f32 = 52.;
/// How far it slides before only the strip is left.
pub(crate) const SLIDE: f32 = HEIGHT - STRIP;
/// Between the banner and what is under it.
pub(crate) const BELOW: f32 = 36.;

/// An album's cover, the part of it that hangs in the banner.
pub(crate) struct Hung {
    /// Where its column starts across the window, and its side.
    pub x: f32,
    pub side: f32,
    /// The covers drawn, the large over the small, each with its opacity.
    pub images: Vec<(Arc<RenderImage>, f32)>,
    pub ground: (Hsla, Hsla),
    /// Away while a cover travels to its place or from it: the banner's
    /// words still stop short of it.
    pub hidden: bool,
}

/// What a page's banner shows.
pub(crate) struct Banner {
    pub title: SharedString,
    pub facts: String,
    /// The way out: to all the artists, or to the record's artist.
    pub up_label: SharedString,
    pub sentence: Vec<Phrase>,
    /// Said quietly: there is no history yet at all.
    pub faint: bool,
    /// Whether there is anything to play yet.
    pub playable: bool,
    /// An artist's face is round; a cover is not.
    pub round: bool,
    /// The picture blurred, or a record's soft colour blurred, and when it came.
    pub backdrop: Option<(Arc<RenderImage>, Instant)>,
    /// The deep colour under the picture while it comes.
    pub color: Option<Rgba>,
    /// What the strip shows small beside the name.
    pub face: Option<Arc<RenderImage>>,
    /// An album's cover, when it heads a column beside the tracks.
    pub hung: Option<Hung>,
    /// An artist's portrait, standing clear at the right, and when it came.
    pub portrait: Option<(Arc<RenderImage>, Instant)>,
    /// The picture's opacity and scale, for an artist's entrance.
    pub picture: (f32, f32),
}

/// What the way out does.
pub(crate) type Up<V> = Rc<dyn Fn(&mut V, &mut Context<V>)>;
/// What Play does, and Shuffle with `true`.
pub(crate) type Play<V> = Rc<dyn Fn(&mut V, bool, &mut Context<V>)>;

/// What the banner's controls do.
pub(crate) struct Actions<V> {
    pub up: Up<V>,
    /// Plays the page's songs, shuffled or not.
    pub play: Play<V>,
}

/// How far along `ms` from `at`, eased out; and whether it is still going.
fn came(at: Instant, ms: f32) -> (f32, bool) {
    let t = (at.elapsed().as_secs_f32() * 1000. / ms).clamp(0., 1.);
    (1. - (1. - t).powi(3), t < 1.)
}

impl Banner {
    /// The banner drawn with the page scrolled `top` points (below zero
    /// while it stretches past its top): where it is, and its fades.
    /// Whether anything in it is still coming in. It runs `width` across,
    /// to the window's edge; what is written on it keeps to the page's
    /// margins, `gutter` at the left and `right` at the right, which is
    /// wider by the room Up next's card takes when it moves the page over.
    #[allow(clippy::too_many_arguments, reason = "the banner's place on the page")]
    pub(crate) fn render<V: 'static>(
        self,
        top: f32,
        width: f32,
        gutter: f32,
        right: f32,
        actions: &Actions<V>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> (AnyElement, bool) {
        let mut animating = false;
        let p = (top / SLIDE).clamp(0., 1.);
        let fade = |value: f32| value.clamp(0., 1.);
        let big_shown = fade(1. - (p - 0.2) * 3.3);
        let strip_shown = fade((p - 0.45) * 3.3);
        let small = p > 0.85;
        // On the picture the words are light, whatever the palette.
        let ink = gpui::rgb(0xf5f5f6);
        let dim = rgba(0xffffffc7);
        let fill = rgba(0xffffff29);
        let dark = gpui::rgb(0x17181b);

        let mut banner = div()
            .absolute()
            .left_0()
            .top(px(-top.min(SLIDE)))
            .w(px(width))
            .h(px(HEIGHT))
            .overflow_hidden()
            .bg(dark)
            // What Up next's card frosts, noted as it is drawn (`frost.rs`).
            .children(frost::piece(frost::Fill::Solid(dark), 1.));
        if let Some(color) = self.color {
            banner = banner
                .child(div().absolute().size_full().bg(color))
                .children(frost::piece(frost::Fill::Solid(color), 1.));
        }
        if let Some((picture, at)) = &self.backdrop {
            // The blur's box runs 60 points past each edge; the picture
            // covers it, at the centre across and 30% down. Arriving at an
            // artist it comes up and eases into place from a little larger,
            // about the banner's centre.
            let (opacity, scale) = self.picture;
            let side = (width + 120.).max(HEIGHT + 120.);
            let left = -60. + (width + 120. - side) / 2.;
            let top = -60. + 0.3 * (HEIGHT + 120. - side);
            let grown = side * scale;
            let (centre_x, centre_y) = (width / 2., HEIGHT / 2.);
            let (shown, going) = came(*at, 300.);
            animating |= going;
            let (x, y) = (
                centre_x + (left - centre_x) * scale,
                centre_y + (top - centre_y) * scale,
            );
            banner = banner
                .child(
                    img(Arc::clone(picture))
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .size(px(grown))
                        .object_fit(ObjectFit::Cover)
                        .opacity(shown * opacity),
                )
                .children(
                    frost::piece(frost::Fill::Picture(Arc::clone(picture)), shown * opacity)
                        .map(|piece| piece.left(px(x)).top(px(y)).size(px(grown))),
                );
        }
        let gloom = |toward, from: u32, to: u32, end| {
            frost::piece(
                frost::Fill::Gradient {
                    toward,
                    from: rgba(from),
                    to: rgba(to),
                    start: 0.,
                    end,
                },
                1.,
            )
        };
        banner = banner
            .children(frost::piece(frost::Fill::Solid(rgba(0x00000066)), 1.))
            .children(gloom(frost::Toward::Right, 0x0c0c_0e9e, 0x0c0c_0e0a, 0.8))
            .children(gloom(frost::Toward::Up, 0x0c0c_0e73, 0x0c0c_0e00, 0.6))
            // `brightness(0.6)`.
            .child(div().absolute().size_full().bg(rgba(0x00000066)))
            // Darker where the words are, so they read on any picture.
            .child(div().absolute().size_full().bg(linear_gradient(
                90.,
                linear_color_stop(rgba(0x0c0c0e9e), 0.),
                linear_color_stop(rgba(0x0c0c0e0a), 0.8),
            )))
            .child(div().absolute().size_full().bg(linear_gradient(
                0.,
                linear_color_stop(rgba(0x0c0c0e73), 0.),
                linear_color_stop(rgba(0x0c0c0e00), 0.6),
            )));

        // An artist's portrait stands clear at the right, its left edge
        // melting into the blur of itself (`melted`), and fades as the strip
        // comes. Gone, it is not drawn at all.
        if let Some((portrait, at)) = &self.portrait {
            let (shown, going) = came(*at, 300.);
            animating |= going;
            let opacity = fade(1. - p * 1.25) * shown;
            if opacity > 0. {
                banner = banner.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .w(px(portrait_wide(width)))
                        .h_full()
                        .opacity(opacity)
                        .child(
                            img(Arc::clone(portrait))
                                .size_full()
                                .object_fit(ObjectFit::Fill),
                        )
                        .children(frost::piece(
                            frost::Fill::Picture(Arc::clone(portrait)),
                            opacity,
                        )),
                );
            }
        }

        // An album's cover hangs out of the banner: this is the part of it
        // in the banner, which fades as the banner becomes its strip.
        let words_right = match &self.hung {
            Some(hung) if hung.hidden => hung.x - 32.,
            Some(hung) => {
                let shown = fade(1. - (p - 0.72) * 3.6);
                let mut cover = div()
                    .absolute()
                    .left(px(hung.x))
                    .top(px(30.))
                    .size(px(hung.side))
                    .rounded(px(12.))
                    .opacity(shown)
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(hung.ground.0, 0.),
                        linear_color_stop(hung.ground.1, 1.),
                    ))
                    .shadow(cover_shadow())
                    .children(frost::piece(frost::ground(hung.ground), shown));
                for (image, opacity) in &hung.images {
                    cover = cover.children(frost::piece(
                        frost::Fill::Picture(Arc::clone(image)),
                        shown * opacity,
                    ));
                    cover = cover.child(
                        img(Arc::clone(image))
                            .absolute()
                            .top_0()
                            .left_0()
                            .size(px(hung.side))
                            .rounded(px(12.))
                            .object_fit(ObjectFit::Cover)
                            .opacity(*opacity),
                    );
                }
                banner = banner.child(cover);
                hung.x - 32.
            }
            None => width - right,
        };

        let up = Rc::clone(&actions.up);
        // Only what is shown takes a click: the large words until the
        // banner is nearly its strip, then the strip.
        let up_button = |id: &'static str, live: bool| {
            let up = Rc::clone(&up);
            div()
                .id(id)
                .h(px(23.))
                .pl(px(3.))
                .pr(px(9.))
                .rounded(px(7.))
                .flex()
                .items_center()
                .gap(px(2.))
                .text_size(px(12.5))
                .font_weight(FontWeight(600.))
                .text_color(dim)
                .hover(|style| style.bg(fill).text_color(ink))
                .child(icon("back", 14., dim))
                .child(self.up_label.clone())
                .when(live, |button| {
                    button.on_click(cx.listener(move |view, _, _, cx| up(view, cx)))
                })
        };
        let play = Rc::clone(&actions.play);
        let pill =
            |label: &'static str, primary: bool, compact: bool, id: &'static str, live: bool| {
                let play = Rc::clone(&play);
                let mut pill = div()
                    .id(id)
                    .h(px(if compact { 30. } else { 36. }))
                    .px(px(if compact { 14. } else { 18. }))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(if compact { 13. } else { 14. }))
                    .font_weight(FontWeight(600.));
                pill = if primary {
                    pill.bg(ink)
                        .text_color(dark)
                        .child(icon("play", 12., dark))
                        .child(label)
                } else {
                    pill.bg(fill).text_color(ink).child(label)
                };
                if !self.playable {
                    pill.opacity(0.45)
                } else if live {
                    pill.on_click(cx.listener(move |view, _, _, cx| play(view, !primary, cx)))
                } else {
                    pill
                }
            };

        // The large words, each row where the real banner has it.
        let long = self.title.chars().count() > 22;
        let (size, line) = if long { (40., 42.) } else { (50., 52.) };
        // The real `h1`'s room for its words: its `max-width` less the 28
        // points of padding at each side that keep its letters' tails and
        // shadow uncut (the page's boxes are `border-box`). Beside a hung
        // cover that is all of `.big`, which ends 32 points short of the
        // cover; otherwise 62% of it.
        let title_room = if self.hung.is_some() {
            words_right - gutter
        } else {
            (words_right - gutter) * 0.62 - 56.
        };
        let heading = tracked(
            self.title.clone(),
            size,
            line,
            FontWeight::BOLD,
            ink,
            -0.034,
        );
        // Too long, it is cut short as WebKit's ellipsis cuts it: at the
        // last letter that still leaves room for "…", the letters tracked
        // as the whole name is.
        let heading = if heading.width(window) <= title_room {
            heading
        } else {
            let set = |text: String| tracked(text, size, line, FontWeight::BOLD, ink, -0.034);
            let ends: Vec<usize> = self
                .title
                .char_indices()
                .map(|(at, _)| at)
                .skip(1)
                .collect();
            let fit = ends.partition_point(|&end| {
                set(format!("{}…", self.title[..end].trim_end())).width(window) <= title_room
            });
            let kept = fit
                .checked_sub(1)
                .and_then(|at| ends.get(at))
                .copied()
                .unwrap_or(0);
            set(format!("{}…", self.title[..kept].trim_end()))
        };
        let title = div()
            .absolute()
            .left(px(gutter))
            .top(px(if long { 50. } else { 45. }))
            .w(px(title_room))
            .child(heading);
        let sentence_color = if self.faint {
            rgba(0xffffffa3)
        } else {
            rgba(0xffffffdb)
        };
        let mut sentence = div()
            .min_w_0()
            .truncate()
            .text_size(px(15.5))
            .line_height(gpui::relative(1.35))
            .text_color(sentence_color);
        // What the sentence turns on, a month or a count, is set apart.
        let text: String = self
            .sentence
            .iter()
            .map(|phrase| phrase.text.as_str())
            .collect();
        let mut highlights = Vec::new();
        let mut at = 0;
        for phrase in &self.sentence {
            let end = at + phrase.text.len();
            if phrase.strong {
                highlights.push((
                    at..end,
                    gpui::HighlightStyle {
                        color: Some(ink.into()),
                        font_weight: Some(FontWeight(500.)),
                        ..Default::default()
                    },
                ));
            }
            at = end;
        }
        sentence = sentence.child(gpui::StyledText::new(text).with_highlights(highlights));
        let big = div()
            .absolute()
            .size_full()
            .opacity(big_shown)
            .child(
                div()
                    .absolute()
                    .left(px(gutter - 5.))
                    .top(px(16.))
                    .child(up_button("banner-up", !small)),
            )
            .child(title)
            .child(
                div()
                    .absolute()
                    .left(px(gutter))
                    .top(px(105.))
                    .text_size(px(14.))
                    .text_color(dim)
                    .font_features(crate::bar::tabular())
                    .child(self.facts.clone()),
            )
            .child(
                div()
                    .absolute()
                    .left(px(gutter + 12.))
                    .top(px(147.))
                    .w(px(words_right - gutter - 12.))
                    .flex()
                    .items_center()
                    .gap(px(22.))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap(px(8.))
                            .child(pill("Play", true, false, "banner-play", !small))
                            .child(pill("Shuffle", false, false, "banner-shuffle", !small))
                            .child(
                                div()
                                    .size(px(36.))
                                    .rounded_full()
                                    .bg(fill)
                                    .opacity(0.45)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon("dots", 14., ink)),
                            ),
                    )
                    .child(sentence),
            );

        // The strip that is left: the way back, the face, the name and Play.
        let face = div()
            .size(px(28.))
            .flex_none()
            .overflow_hidden()
            .when(self.round, |face| face.rounded_full())
            .when(!self.round, |face| face.rounded(px(5.)))
            .bg(fill)
            .children(self.face.clone().map(|face| {
                img(face)
                    .size(px(28.))
                    .object_fit(ObjectFit::Cover)
                    .when(self.round, |face| face.rounded_full())
                    .when(!self.round, |face| face.rounded(px(5.)))
            }));
        let strip = div()
            .absolute()
            .left(px(gutter))
            .top(px(SLIDE))
            .w(px(width - gutter - right))
            .h(px(STRIP))
            .flex()
            .items_center()
            .gap(px(10.))
            .opacity(strip_shown)
            .child(
                div()
                    .ml(px(-5.))
                    .mr(px(6.))
                    .child(up_button("strip-up", small)),
            )
            .child(face)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(15.))
                    .font_weight(FontWeight(650.))
                    .text_color(ink)
                    .child(self.title.clone()),
            )
            .child(div().flex_1())
            .child(
                div()
                    .ml(px(2.))
                    .child(pill("Play", true, true, "strip-play", small)),
            )
            .child(
                div()
                    .ml(px(2.))
                    .child(pill("Shuffle", false, true, "strip-shuffle", small)),
            );

        (banner.child(big).child(strip).into_any_element(), animating)
    }
}

/// How much of the portrait's width, from its left edge, melts into the
/// blur (`mask-image: linear-gradient(90deg, transparent, #000 52%)`).
const MELT: f32 = 0.52;

/// How wide an artist's portrait stands in a banner `width` points across.
pub(crate) fn portrait_wide(width: f32) -> f32 {
    (width * 0.4).min(380.)
}

/// Where a picture `iw` by `ih` lies in a box `w` by `h`, its left and top
/// and its size: it covers the box, kept at the middle across and 30% down
/// (`object-fit: cover; object-position: center 30%`).
fn placed(iw: f32, ih: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let (iw, ih) = (iw.max(1.), ih.max(1.));
    let scale = (w / iw).max(h / ih);
    let (dw, dh) = (iw * scale, ih * scale);
    ((w - dw) * 0.5, (h - dh) * 0.3, dw, dh)
}

/// An artist's portrait as its box in the banner shows it, `w` by `h`
/// device pixels: cropped as [`placed`] places it, and its left edge melting
/// to nothing, the mask's ramp made its alpha. GPUI has no mask, so the
/// ramp is in the picture, which is drawn whole and exact at any scale. In
/// BGRA, as GPUI takes it.
pub(crate) fn melted(source: &RgbaImage, w: u32, h: u32) -> RenderImage {
    let (w, h) = (w.max(1), h.max(1));
    let (sw, sh) = (source.width().max(1), source.height().max(1));
    let (x, y, dw, _) = placed(sw as f32, sh as f32, w as f32, h as f32);
    // The part of the picture the box shows, in the picture's own pixels.
    let scale = dw / sw as f32;
    let left = ((-x / scale).round() as u32).min(sw - 1);
    let top = ((-y / scale).round() as u32).min(sh - 1);
    let across = ((w as f32 / scale).round() as u32).clamp(1, sw - left);
    let down = ((h as f32 / scale).round() as u32).clamp(1, sh - top);
    let part = image::imageops::crop_imm(source, left, top, across, down).to_image();
    let mut out = image::imageops::resize(&part, w, h, image::imageops::FilterType::Triangle);
    let melt = w as f32 * MELT;
    for (column, _, pixel) in out.enumerate_pixels_mut() {
        let [r, g, b, a] = pixel.0;
        let shown = ((column as f32 + 0.5) / melt).min(1.);
        pixel.0 = [b, g, r, (f32::from(a) * shown).round() as u8];
    }
    RenderImage::new(vec![image::Frame::new(out)])
}

/// The real cover's shadow, in each palette.
pub(crate) fn cover_shadow() -> Vec<BoxShadow> {
    vec![if theme::dark() {
        theme::css_shadow(22., 54., theme::cover_shadow())
    } else {
        theme::css_shadow(16., 40., theme::cover_shadow())
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_portrait_covers_its_box_kept_30_percent_down() {
        let near = |a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)| {
            (a.0 - b.0).abs() < 1e-3
                && (a.1 - b.1).abs() < 1e-3
                && (a.2 - b.2).abs() < 1e-3
                && (a.3 - b.3).abs() < 1e-3
        };
        // Square, and tall: as wide as the box, cut 30% of the way down.
        let square = placed(1000., 1000., 380., HEIGHT);
        assert!(
            near(square, (0., 0.3 * (HEIGHT - 380.), 380., 380.)),
            "{square:?}"
        );
        let tall = placed(600., 1200., 380., HEIGHT);
        assert!(
            near(tall, (0., 0.3 * (HEIGHT - 760.), 380., 760.)),
            "{tall:?}"
        );
        // Wide: as tall as the box, kept at the middle across.
        let wide = placed(1200., 600., 380., HEIGHT);
        let across = 2. * HEIGHT;
        assert!(
            near(wide, ((380. - across) / 2., 0., across, HEIGHT)),
            "{wide:?}"
        );
    }

    #[test]
    fn the_melt_is_the_pictures_alpha_from_clear_to_whole() {
        // A tall picture, red over blue: the box shows only red, as it is
        // cut 30% down at its own shape, and squashed square it would not.
        let source = RgbaImage::from_fn(100, 200, |_, y| {
            if y < 100 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        let made = melted(&source, 380, 214);
        let size = made.size(0);
        assert_eq!((size.width.0, size.height.0), (380, 214));
        let bytes = made.as_bytes(0).unwrap_or_default();
        let at = |x: usize, y: usize| bytes.get((y * 380 + x) * 4..(y * 380 + x) * 4 + 4);
        let mut before = 0;
        for x in 0..380 {
            // A pixel missing reads as black, and fails below.
            let [b, _, r, a] = at(x, 213)
                .and_then(|pixel| <[u8; 4]>::try_from(pixel).ok())
                .unwrap_or_default();
            // In BGRA, red, at every row down to the box's foot.
            assert!(r > 240 && b < 15, "{x}: {r} {b}");
            assert!(a >= before, "{x}: {a} after {before}");
            before = a;
            if x as f32 >= 380. * MELT {
                assert_eq!(a, 255, "{x}");
            }
        }
        assert!(
            at(0, 0)
                .and_then(|pixel| pixel.get(3).copied())
                .is_some_and(|alpha| alpha <= 2),
            "{:?}",
            at(0, 0)
        );
    }
}
