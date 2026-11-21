//! The frost behind Up next's card, faked. The real card blurs what is
//! behind it (`backdrop-filter: blur(40px) saturate(1.5)`, `UpNext.svelte`);
//! GPUI has no backdrop blur, so the card paints under its fill a picture
//! of what lies behind it, made on the CPU from what the window drew there:
//! the window's colour, the wash and its veil, a page's banner, covers and
//! History's picture. Each is noted here as it is laid out (`piece`), in the
//! window's points, clipped as it is drawn and as faded as it is shown
//! (`faded`); the card then paints them small, blurs them as WebKit does
//! and saturates them, and draws the result stretched, rounded as it is.
//! Words and hairlines are left out: blurred 40 points, they are no more
//! than a breath of colour.
//!
//! WebKit's backdrop takes only what lies inside the card, its edges
//! repeated outward (measured: the title bar above it does not lighten its
//! top), so the picture covers the card alone.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Bounds, Canvas, Corners, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, RenderImage, Rgba, Styled, Window, canvas,
    point, px, size,
};

/// The card's blur, and its saturation (`UpNext.svelte`).
pub(crate) const CARD: (f32, f32) = (40., 1.5);

/// A ground drawn from one colour at the top to another at the bottom.
pub(crate) fn ground((from, to): (gpui::Hsla, gpui::Hsla)) -> Fill {
    Fill::Gradient {
        toward: Toward::Down,
        from: from.into(),
        to: to.into(),
        start: 0.,
        end: 1.,
    }
}

/// How a piece is filled.
#[derive(Clone)]
pub(crate) enum Fill {
    /// One colour, its alpha as given.
    Solid(Rgba),
    /// A gradient from one colour to another, between two shares of the
    /// piece's height or width (`linear_gradient` with two stops).
    Gradient {
        toward: Toward,
        from: Rgba,
        to: Rgba,
        start: f32,
        end: f32,
    },
    /// A picture, covering the piece.
    Picture(Arc<RenderImage>),
    /// History's picture, and each layer's colour.
    Stream(Rc<crate::stream::Drawn>, Rc<Vec<Rgba>>),
}

/// Which way a gradient runs, as `linear_gradient`'s angles: 180, 0 and 90.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Toward {
    Down,
    Up,
    Right,
}

impl PartialEq for Fill {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Solid(a), Self::Solid(b)) => a == b,
            (
                Self::Gradient {
                    toward,
                    from,
                    to,
                    start,
                    end,
                },
                Self::Gradient {
                    toward: toward_b,
                    from: from_b,
                    to: to_b,
                    start: start_b,
                    end: end_b,
                },
            ) => {
                toward == toward_b
                    && from == from_b
                    && to == to_b
                    && start == start_b
                    && end == end_b
            }
            (Self::Picture(a), Self::Picture(b)) => a.id == b.id,
            (Self::Stream(a, colors), Self::Stream(b, colors_b)) => {
                Rc::ptr_eq(a, b) && colors == colors_b
            }
            _ => false,
        }
    }
}

/// One thing drawn behind the card: where it is drawn, what of it is seen,
/// and how.
#[derive(Clone, PartialEq)]
struct Piece {
    rect: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    fill: Fill,
    opacity: f32,
}

thread_local! {
    /// Whether the card is shown, and so wants to know what is behind it.
    static WANTED: Cell<bool> = const { Cell::new(false) };
    /// What was drawn this frame, in the order it was drawn.
    static PIECES: RefCell<Vec<Piece>> = const { RefCell::new(Vec::new()) };
    /// The opacities the pieces now noted are seen through.
    static FADES: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
    /// Each picture small, premultiplied, by its image.
    static SMALL: RefCell<HashMap<gpui::ImageId, Rc<Small>>> = RefCell::new(HashMap::new());
    /// The last frost painted: what it was made from, and the picture.
    static LAST: RefCell<Option<Made>> = const { RefCell::new(None) };
    /// How many times it was made while the card was shown, the longest
    /// and in all, in milliseconds, for the developer's line.
    static COST: Cell<(u32, f32, f32)> = const { Cell::new((0, 0., 0.)) };
}

struct Made {
    bounds: Bounds<Pixels>,
    pieces: Vec<Piece>,
    dark: bool,
    image: Arc<RenderImage>,
}

/// A new frame: forgets what the last one drew. The shell calls it first,
/// saying whether the card is shown.
pub(crate) fn begin(wanted: bool) {
    WANTED.set(wanted);
    PIECES.with_borrow_mut(Vec::clear);
}

/// Whether anything need be noted.
pub(crate) fn wanted() -> bool {
    WANTED.get()
}

/// Notes a piece drawn at `rect` in the window, seen inside `clip`.
pub(crate) fn note(rect: Bounds<Pixels>, clip: Bounds<Pixels>, fill: Fill, opacity: f32) {
    if !wanted() {
        return;
    }
    let faded: f32 = FADES.with_borrow(|fades| fades.iter().product());
    let opacity = opacity * faded;
    if opacity <= 0.002 {
        return;
    }
    PIECES.with_borrow_mut(|pieces| {
        pieces.push(Piece {
            rect,
            clip,
            fill,
            opacity,
        });
    });
}

/// A piece that notes where it is laid out: absolute and as large as what
/// holds it, unless styled otherwise. `None` while the card is not shown.
pub(crate) fn piece(fill: Fill, opacity: f32) -> Option<Canvas<()>> {
    wanted().then(|| {
        canvas(
            move |bounds, window, _| {
                let clip = window.content_mask().bounds.intersect(&bounds);
                note(bounds, clip, fill, opacity);
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    })
}

/// What is inside seen through `opacity`, for the pieces in it: GPUI's
/// opacity is its own, so a page fading out under the card says so here too.
pub(crate) fn faded(opacity: f32, child: impl IntoElement) -> Faded {
    Faded {
        opacity,
        child: child.into_any_element(),
    }
}

pub(crate) struct Faded {
    opacity: f32,
    child: AnyElement,
}

impl Element for Faded {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        FADES.with_borrow_mut(|fades| fades.push(self.opacity));
        self.child.prepaint(window, cx);
        FADES.with_borrow_mut(|fades| fades.pop());
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// The frost, painted over `bounds` with corners of `radius`: made again
/// only when what is behind it has changed.
pub(crate) fn paint(bounds: Bounds<Pixels>, radius: f32, window: &mut Window) {
    let pieces: Vec<Piece> = PIECES.with_borrow(|pieces| {
        pieces
            .iter()
            .filter(|piece| piece.clip.intersects(&bounds))
            .cloned()
            .collect()
    });
    let dark = crate::theme::dark();
    let reused = LAST.with_borrow(|last| {
        last.as_ref()
            .filter(|made| made.bounds == bounds && made.dark == dark && made.pieces == pieces)
            .map(|made| Arc::clone(&made.image))
    });
    let image = match reused {
        Some(image) => image,
        None => {
            let began = std::time::Instant::now();
            let image = Arc::new(make(bounds, &pieces));
            let ms = began.elapsed().as_secs_f32() * 1000.;
            let (count, longest, all) = COST.get();
            COST.set((count + 1, longest.max(ms), all + ms));
            if let Some(old) = LAST.take() {
                let _ = window.drop_image(old.image);
            }
            LAST.set(Some(Made {
                bounds,
                pieces,
                dark,
                image: Arc::clone(&image),
            }));
            image
        }
    };
    // One cell more on every side, its edges repeated, so that the picture
    // stretched is never mixed with its neighbours in GPUI's atlas.
    let (columns, rows) = cells(bounds);
    let (w, h) = (
        f32::from(bounds.size.width) / columns as f32,
        f32::from(bounds.size.height) / rows as f32,
    );
    let outer = Bounds::new(
        point(bounds.origin.x - px(w), bounds.origin.y - px(h)),
        size(
            bounds.size.width + px(2. * w),
            bounds.size.height + px(2. * h),
        ),
    );
    let _ = window.paint_image(bounds, outer, Corners::all(px(radius)), image, 0, false);
}

/// The frost is gone: its picture is let go.
pub(crate) fn forget(window: &mut Window) {
    if let Some(old) = LAST.take() {
        let _ = window.drop_image(old.image);
    }
    let (count, longest, all) = COST.take();
    if count > 0 {
        crate::debug(format_args!(
            "frost: made {count} times, longest {longest:.2} ms, {:.2} ms on average",
            all / count as f32
        ));
    }
}

/// How many cells across and down the picture has: about 10 points each,
/// a quarter of the blur's deviation, plenty for something so soft.
fn cells(bounds: Bounds<Pixels>) -> (usize, usize) {
    let cell = 10.;
    (
        ((f32::from(bounds.size.width) / cell).round() as usize).max(2),
        ((f32::from(bounds.size.height) / cell).round() as usize).max(2),
    )
}

/// A picture small, premultiplied, for sampling.
struct Small {
    side: (usize, usize),
    pixels: Vec<[f32; 4]>,
}

/// The side a picture is made small to.
const SMALL_SIDE: usize = 24;

fn small(image: &RenderImage) -> Option<Rc<Small>> {
    if let Some(made) = SMALL.with_borrow(|small| small.get(&image.id).cloned()) {
        return Some(made);
    }
    let bytes = image.as_bytes(0)?;
    let full = image.size(0);
    let (iw, ih) = (full.width.0.max(1) as usize, full.height.0.max(1) as usize);
    // At most SMALL_SIDE across its longer side, and of its own shape, so
    // that a picture that is not square is fitted as it is drawn.
    let (sw, sh) = if iw >= ih {
        let sw = SMALL_SIDE.min(iw);
        (sw, (sw * ih / iw).max(1))
    } else {
        let sh = SMALL_SIDE.min(ih);
        ((sh * iw / ih).max(1), sh)
    };
    let mut sums = vec![[0f32; 4]; sw * sh];
    let mut counts = vec![0f32; sw * sh];
    for (i, pixel) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (i % iw, i / iw);
        let at = (y * sh / ih) * sw + x * sw / iw;
        let alpha = f32::from(pixel.get(3).copied().unwrap_or(255)) / 255.;
        // BGRA.
        let channel = |c: usize| f32::from(pixel.get(c).copied().unwrap_or(0)) / 255. * alpha;
        if let (Some(sum), Some(count)) = (sums.get_mut(at), counts.get_mut(at)) {
            *sum = [
                sum[0] + channel(2),
                sum[1] + channel(1),
                sum[2] + channel(0),
                sum[3] + alpha,
            ];
            *count += 1.;
        }
    }
    let pixels = sums
        .iter()
        .zip(&counts)
        .map(|(sum, count)| sum.map(|value| value / count.max(1.)))
        .collect();
    let made = Rc::new(Small {
        side: (sw, sh),
        pixels,
    });
    SMALL.with_borrow_mut(|small| {
        // Pictures come and go as pages do; a few hundred are kept.
        if small.len() > 512 {
            small.clear();
        }
        small.insert(image.id, Rc::clone(&made));
    });
    Some(made)
}

impl Small {
    /// Premultiplied colour at (u, v), shares of its width and height,
    /// between its pixels' centres.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let (w, h) = self.side;
        let x = (u * w as f32 - 0.5).clamp(0., (w - 1) as f32);
        let y = (v * h as f32 - 0.5).clamp(0., (h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let at = |x: usize, y: usize| self.pixels.get(y * w + x).copied().unwrap_or_default();
        let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
        lerp(lerp(a, b, fx), lerp(c, d, fx), fy)
    }
}

/// The picture behind the card, blurred and saturated, a cell to a pixel
/// with a border of one, as BGRA.
fn make(bounds: Bounds<Pixels>, pieces: &[Piece]) -> RenderImage {
    let (columns, rows) = cells(bounds);
    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (cw, ch) = (
        f32::from(bounds.size.width) / columns as f32,
        f32::from(bounds.size.height) / rows as f32,
    );
    let bg = crate::theme::bg();
    let mut field = vec![[bg.r * 255., bg.g * 255., bg.b * 255.]; columns * rows];
    for piece in pieces {
        lay(&mut field, (columns, rows), (x0, y0), (cw, ch), piece);
    }
    let (sigma, saturation) = CARD;
    blur(&mut field, (columns, rows), sigma / cw, true);
    blur(&mut field, (columns, rows), sigma / ch, false);
    let (w, h) = (columns + 2, rows + 2);
    let mut out = image::RgbaImage::new(w as u32, h as u32);
    for (i, target) in out.pixels_mut().enumerate() {
        let (x, y) = ((i % w).clamp(1, columns) - 1, (i / w).clamp(1, rows) - 1);
        let [r, g, b] = saturate(
            field.get(y * columns + x).copied().unwrap_or_default(),
            saturation,
        );
        *target = image::Rgba([b, g, r, 255]);
    }
    RenderImage::new(vec![image::Frame::new(out)])
}

/// Lays one piece over the cells, each cell covered as much as the piece's
/// clip covers it, so that a piece moving moves smoothly.
fn lay(
    field: &mut [[f32; 3]],
    (columns, rows): (usize, usize),
    (x0, y0): (f32, f32),
    (cw, ch): (f32, f32),
    piece: &Piece,
) {
    let clip = piece.clip;
    let (left, top) = (f32::from(clip.origin.x), f32::from(clip.origin.y));
    let (right, bottom) = (
        left + f32::from(clip.size.width),
        top + f32::from(clip.size.height),
    );
    let first = |from: f32, cell: f32| ((from / cell).floor().max(0.)) as usize;
    let (c0, c1) = (
        first(left - x0, cw),
        (((right - x0) / cw).ceil().max(0.) as usize).min(columns),
    );
    let (r0, r1) = (
        first(top - y0, ch),
        (((bottom - y0) / ch).ceil().max(0.) as usize).min(rows),
    );
    let rect = piece.rect;
    let (rx, ry) = (f32::from(rect.origin.x), f32::from(rect.origin.y));
    let (rw, rh) = (
        f32::from(rect.size.width).max(0.01),
        f32::from(rect.size.height).max(0.01),
    );
    let small = match &piece.fill {
        Fill::Picture(image) => {
            let Some(small) = small(image) else { return };
            Some(small)
        }
        _ => None,
    };
    // A picture covers its box, its middle kept.
    let fit = small.as_ref().map(|small| {
        let (iw, ih) = (small.side.0 as f32, small.side.1 as f32);
        let scale = (rw / iw).max(rh / ih);
        let (dw, dh) = (iw * scale, ih * scale);
        (rx + (rw - dw) / 2., ry + (rh - dh) / 2., dw, dh)
    });
    for row in r0..r1 {
        let (cy0, cy1) = (y0 + row as f32 * ch, y0 + (row + 1) as f32 * ch);
        let over_y = (cy1.min(bottom) - cy0.max(top)).max(0.) / ch;
        if over_y <= 0. {
            continue;
        }
        let cy = (cy0 + cy1) / 2.;
        for column in c0..c1 {
            let (cx0, cx1) = (x0 + column as f32 * cw, x0 + (column + 1) as f32 * cw);
            let over_x = (cx1.min(right) - cx0.max(left)).max(0.) / cw;
            if over_x <= 0. {
                continue;
            }
            let cx = (cx0 + cx1) / 2.;
            let covered = over_x * over_y * piece.opacity;
            // Premultiplied, 0 to 255.
            let color: [f32; 4] = match &piece.fill {
                Fill::Solid(color) => premultiplied(*color),
                Fill::Gradient {
                    toward,
                    from,
                    to,
                    start,
                    end,
                } => {
                    let along = match toward {
                        Toward::Down => (cy - ry) / rh,
                        Toward::Up => 1. - (cy - ry) / rh,
                        Toward::Right => (cx - rx) / rw,
                    };
                    let t = ((along - start) / (end - start).max(0.0001)).clamp(0., 1.);
                    let (a, b) = (premultiplied(*from), premultiplied(*to));
                    lerp(a, b, t)
                }
                Fill::Picture(_) => match (&small, fit) {
                    (Some(small), Some((fx, fy, fw, fh))) => {
                        small.sample((cx - fx) / fw, (cy - fy) / fh)
                    }
                    _ => continue,
                },
                Fill::Stream(drawn, colors) => {
                    let mut seen = [0f32; 4];
                    let at = (cx - rx) / drawn.step.max(0.01);
                    for (layer, color) in drawn.layers.iter().zip(colors.iter()) {
                        // The edges between months straight, as drawn, and
                        // level past the first and the last.
                        let edge = |values: &[f32]| {
                            let last = values.len().checked_sub(1)?;
                            let i = (at.floor().max(0.) as usize).min(last);
                            let f = (at - i as f32).clamp(0., 1.);
                            let a = values.get(i).copied()?;
                            let b = values.get(i + 1).copied().unwrap_or(a);
                            Some(a + (b - a) * f)
                        };
                        let (Some(t), Some(b)) = (edge(&layer.top), edge(&layer.bottom)) else {
                            continue;
                        };
                        let share =
                            ((cy1.min(ry + b) - cy0.max(ry + t)).max(0.) / ch).clamp(0., 1.);
                        let c = premultiplied(*color);
                        for i in 0..4 {
                            if let (Some(s), Some(v)) = (seen.get_mut(i), c.get(i)) {
                                *s += v * share;
                            }
                        }
                    }
                    seen
                }
            };
            if let Some(cell) = field.get_mut(row * columns + column) {
                let alpha = color[3] * covered;
                *cell = std::array::from_fn(|i| {
                    cell.get(i).copied().unwrap_or(0.) * (1. - alpha)
                        + color.get(i).copied().unwrap_or(0.) * 255. * covered
                });
            }
        }
    }
}

/// Between two colours.
fn lerp<const N: usize>(a: [f32; N], b: [f32; N], t: f32) -> [f32; N] {
    let mut out = a;
    for (value, to) in out.iter_mut().zip(b) {
        *value += (to - *value) * t;
    }
    out
}

/// A colour premultiplied: its channels, 0 to 1, times its alpha.
fn premultiplied(color: Rgba) -> [f32; 4] {
    [
        color.r * color.a,
        color.g * color.a,
        color.b * color.a,
        color.a,
    ]
}

/// A Gaussian blur of deviation `sigma` cells along one axis, the edges
/// repeated outward.
fn blur(field: &mut [[f32; 3]], (columns, rows): (usize, usize), sigma: f32, across: bool) {
    let reach = (sigma * 3.).ceil() as isize;
    let weights: Vec<f32> = (-reach..=reach)
        .map(|i| (-(i * i) as f32 / (2. * sigma * sigma)).exp())
        .collect();
    let total: f32 = weights.iter().sum();
    let (lines, length) = if across {
        (rows, columns)
    } else {
        (columns, rows)
    };
    let at = |line: usize, i: usize| {
        if across {
            line * columns + i
        } else {
            i * columns + line
        }
    };
    let mut line_values = vec![[0f32; 3]; length];
    for line in 0..lines {
        for (i, value) in line_values.iter_mut().enumerate() {
            *value = field.get(at(line, i)).copied().unwrap_or_default();
        }
        for i in 0..length {
            let mut sum = [0f32; 3];
            for (k, weight) in weights.iter().enumerate() {
                let j = (i as isize + k as isize - reach).clamp(0, length as isize - 1) as usize;
                let v = line_values.get(j).copied().unwrap_or_default();
                for (total, value) in sum.iter_mut().zip(v) {
                    *total += value * weight;
                }
            }
            if let Some(cell) = field.get_mut(at(line, i)) {
                *cell = sum.map(|value| value / total);
            }
        }
    }
}

/// CSS's `saturate(s)`, its matrix as the filter effects spec gives it.
fn saturate([r, g, b]: [f32; 3], s: f32) -> [u8; 3] {
    let out = [
        (0.213 + 0.787 * s) * r + (0.715 - 0.715 * s) * g + (0.072 - 0.072 * s) * b,
        (0.213 - 0.213 * s) * r + (0.715 + 0.285 * s) * g + (0.072 - 0.072 * s) * b,
        (0.213 - 0.213 * s) * r + (0.715 - 0.715 * s) * g + (0.072 + 0.928 * s) * b,
    ];
    out.map(|value| value.round().clamp(0., 255.) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn a_picture_made_small_keeps_its_shape() {
        let picture =
            |w: u32, h: u32| RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(w, h))]);
        let side = |image: &RenderImage| small(image).map(|small| small.side);
        assert_eq!(side(&picture(1000, 1000)), Some((24, 24)));
        assert_eq!(side(&picture(760, 428)), Some((24, 13)));
        assert_eq!(side(&picture(300, 600)), Some((12, 24)));
        assert_eq!(side(&picture(10, 6)), Some((10, 6)));
    }

    #[test]
    fn grey_is_left_grey_by_saturation() {
        assert_eq!(saturate([120., 120., 120.], 1.5), [120, 120, 120]);
        let [r, _, b] = saturate([150., 120., 100.], 1.5);
        assert!(r > 150 && b < 100);
    }

    #[test]
    fn a_band_melts_over_about_a_deviation_each_side() {
        // A black band over the card's top half, on white.
        let card = at(0., 0., 320., 800.);
        let band = Piece {
            rect: at(0., 0., 320., 400.),
            clip: at(0., 0., 320., 400.),
            fill: Fill::Solid(Rgba {
                r: 0.,
                g: 0.,
                b: 0.,
                a: 1.,
            }),
            opacity: 1.,
        };
        let white = Piece {
            rect: card,
            clip: card,
            fill: Fill::Solid(Rgba {
                r: 1.,
                g: 1.,
                b: 1.,
                a: 1.,
            }),
            opacity: 1.,
        };
        let (columns, rows) = cells(card);
        let mut field = vec![[0f32; 3]; columns * rows];
        for piece in [&white, &band] {
            lay(&mut field, (columns, rows), (0., 0.), (10., 10.), piece);
        }
        blur(&mut field, (columns, rows), 4., false);
        let row = |y: usize| field.get(y * columns).map(|cell| cell[0]).unwrap_or(-1.);
        // Black well inside, white well outside, half way at the edge.
        assert!(row(5) < 1.);
        assert!(row(75) > 254.);
        assert!((row(39) - 127.5).abs() < 20. && (row(40) - 127.5).abs() < 20.);
        // A deviation (40 points, 4 cells) below the edge, about 84% white.
        assert!((row(44) / 255. - 0.84).abs() < 0.05);
    }
}
