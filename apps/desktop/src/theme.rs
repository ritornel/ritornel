//! The desktop palettes ("Frame and navigation" in
//! docs/internals/desktop.md): Graphite (dark) and Mist (light). The window
//! follows the system's appearance and accent; there is no switch yet.
//!
//! What the system says comes in through `follow` and `follow_accent`, and
//! the tokens are worked out from it there, once, not as each is drawn. A
//! choice made in the app's own settings would come in the same way, in
//! place of the system's.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use gpui::{Hsla, Rgba, StyleRefinement, Styled, WindowAppearance, hsla, px, rgba};

use crate::accent::{self, Look};

static DARK: AtomicBool = AtomicBool::new(true);

/// Follows the system's appearance.
/// `RITORNEL_GPUI_APPEARANCE=light` or `dark` holds one, for a developer's
/// pictures, without changing the system's.
pub(crate) fn follow(appearance: WindowAppearance) {
    let dark = match std::env::var("RITORNEL_GPUI_APPEARANCE").as_deref() {
        Ok("light") => false,
        Ok("dark") => true,
        _ => matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
    };
    DARK.store(dark, Ordering::Relaxed);
}

pub(crate) fn dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// A token: dark first, light second, as `0xrrggbbaa`.
type Token = (u32, u32);

/// The token for this appearance.
fn pick((dark, light): Token) -> Rgba {
    if self::dark() {
        rgba(dark)
    } else {
        rgba(light)
    }
}

const BG: Token = (0x1b1c1fff, 0xf7f8faff);
const BAR_BG: Token = (0x202125ff, 0xf1f3f6ff);
const SURF: Token = (0x232428ff, 0xedeff3ff);
const RAISED: Token = (0x2d2e33ff, 0xffffffff);
/// The app's own accent, where the system leaves it one.
const ACC: Token = (0x8fb4d9ff, 0x5a61d6ff);

pub(crate) fn bg() -> Rgba {
    pick(BG)
}
pub(crate) fn bar_bg() -> Rgba {
    pick(BAR_BG)
}
/// A surface over the page, as Up next's card is.
pub(crate) fn surf() -> Rgba {
    pick(SURF)
}
pub(crate) fn raised() -> Rgba {
    pick(RAISED)
}
/// A neutral control's ground, as a secondary button's.
const FILL: Token = (0xffffff12, 0x1c22300f);

pub(crate) fn fill() -> Rgba {
    pick(FILL)
}
pub(crate) fn fill2() -> Rgba {
    pick((0xffffff26, 0x1c223024))
}
pub(crate) fn line() -> Rgba {
    pick((0xffffff17, 0x1c22301a))
}
const INK: Token = (0xe9e9ecff, 0x1c2230ff);
const DIM: Token = (0xa8a9b0ff, 0x5a6273ff);
const FAINT: Token = (0x8d8e97ff, 0x616979ff);

pub(crate) fn ink() -> Rgba {
    pick(INK)
}
pub(crate) fn dim() -> Rgba {
    pick(DIM)
}
pub(crate) fn faint() -> Rgba {
    pick(FAINT)
}

// ---- Status ("Status" in docs/internals/desktop.md). A status's colour
// says whether it is fine, being worked on, waiting on you, or wrong; its
// shape says whether it is here (`marks::Mark`), and its words always say
// it too. A mark (a dot, a ring, a badge) is held to 3:1 on the palette's
// grounds and text to 4.5:1 on them and on a neutral button's fill, so each
// colour has a mark and a text form. None of them follows the accent.

/// Fine: here, and nothing to do.
const OK: Token = (0x3aae86ff, 0x1f8f64ff);
const OK_TEXT: Token = (0x3aae86ff, 0x17704eff);
/// Waiting on you, with nothing lost yet.
const NEED: Token = (0xe0ad48ff, 0xc2820aff);
const NEED_TEXT: Token = (0xe0ad48ff, 0x8f6207ff);
const NEED_TINT: Token = (0xd9a44121, 0xc2820a1f);
/// Failed, or losing something until something changes.
const FAIL: Token = (0xee6a5fff, 0xd23f32ff);
const FAIL_TEXT: Token = (0xf47c72ff, 0xb52b20ff);
const FAIL_TINT: Token = (0xee6a5f1f, 0xd23f3217);
/// Being done now: the app's own accent, kept when `acc` follows the
/// system's, so that a red or green accent never reads as a status.
const WORKING: Token = ACC;

/// A fine status's mark.
pub(crate) fn ok() -> Rgba {
    pick(OK)
}
/// A fine status in words.
pub(crate) fn ok_text() -> Rgba {
    pick(OK_TEXT)
}
/// The mark of what waits on you.
pub(crate) fn need() -> Rgba {
    pick(NEED)
}
/// What waits on you, in words.
pub(crate) fn need_text() -> Rgba {
    pick(NEED_TEXT)
}
/// The ground of a box that says what waits on you.
#[allow(dead_code, reason = "no box says what waits on you yet")]
pub(crate) fn need_tint() -> Rgba {
    pick(NEED_TINT)
}
/// The mark of what went wrong.
pub(crate) fn fail() -> Rgba {
    pick(FAIL)
}
/// What went wrong, in words.
pub(crate) fn fail_text() -> Rgba {
    pick(FAIL_TEXT)
}
/// The ground of a box that says what went wrong.
#[allow(dead_code, reason = "no box says what went wrong yet")]
pub(crate) fn fail_tint() -> Rgba {
    pick(FAIL_TINT)
}
/// What is being done now: the open arc that turns (`marks::Mark`).
pub(crate) fn working() -> Rgba {
    pick(WORKING)
}

// ---- Buttons that carry colour (`button.rs`). Each keeps its words at 4.5:1
// under the pointer too, so neither fades as a solid control does.

/// Words on the accent: the page's ground in the dark, where the accent is
/// light, and white in the light, where it is deep. The accent is worked out
/// to 4.5:1 on both (`accent::derive`).
const ON_ACCENT: Token = (BG.0, RAISED.1);
/// How far a primary button's accent moves toward the ink under the
/// pointer: away from its words, which are the ink's opposite.
const PRIMARY_HOVER: f32 = 0.12;
/// How much red a red thing takes as its ground under the pointer or the
/// keys, a destructive button or a place whose output failed: as much as
/// keeps its words at 4.5:1, and its mark at 3:1, on every ground. The
/// neutral `fill` would not, on a raised surface in the dark.
const FAIL_HOVER: f32 = 0.08;

pub(crate) fn on_accent() -> Rgba {
    pick(ON_ACCENT)
}
/// A primary button's ground under the pointer.
pub(crate) fn primary_hover() -> Rgba {
    mix(acc(), ink(), PRIMARY_HOVER)
}
/// The ground of a red thing under the pointer or the keys (`FAIL_HOVER`).
pub(crate) fn fail_hover() -> Rgba {
    Rgba {
        a: FAIL_HOVER,
        ..fail()
    }
}

/// What a status says, by its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// Nothing to do: here and not the app's, or away as expected.
    Quiet,
    /// Here, and nothing to do.
    Ok,
    /// Being done now.
    Working,
    /// Waiting on you, and nothing is lost yet.
    Need,
    /// It failed, or is losing something until something changes.
    Fail,
}

impl Status {
    /// Its mark's colour.
    pub(crate) fn mark(self) -> Rgba {
        match self {
            Self::Quiet => faint(),
            Self::Ok => ok(),
            Self::Working => working(),
            Self::Need => need(),
            Self::Fail => fail(),
        }
    }

    /// Its words' colour. What is being done is said plainly, beside its
    /// arc.
    pub(crate) fn text(self) -> Rgba {
        match self {
            Self::Quiet | Self::Working => dim(),
            Self::Ok => ok_text(),
            Self::Need => need_text(),
            Self::Fail => fail_text(),
        }
    }
}
/// What is lit: the system's accent as the palette wears it (`accent.rs`),
/// or the app's own.
pub(crate) fn acc() -> Rgba {
    let [dark, light] = &ACCENT;
    pick((dark.load(Ordering::Relaxed), light.load(Ordering::Relaxed)))
}
/// The system's accent as its own controls wear it, for the controls drawn
/// to look like them (`settings/controls.rs`), darkened where their white
/// knob would not show on it (`accent::control`): the system's default
/// where it names none (`platform::CONTROL_ACCENT`).
pub(crate) fn system_accent() -> Rgba {
    rgba(SYSTEM_ACCENT.load(Ordering::Relaxed))
}
/// Under a large cover: `--cover-shadow`.
pub(crate) fn cover_shadow() -> Rgba {
    pick((0x00000061, 0x28261e38))
}

/// The share of the accent in the ground of what is lit: a button of the
/// player bar (`bar.rs`), the title bar's tab (`frame.rs`).
pub(crate) const LIT_TINT: f32 = 0.14;

/// The ground of what is lit: the accent at `LIT_TINT`, over whatever
/// ground it is on.
pub(crate) fn lit() -> Rgba {
    Rgba {
        a: LIT_TINT,
        ..acc()
    }
}

/// The words of what is lit among words in `dim`, as the title bar's tab
/// is: the accent, or the ink where the accent is a grey, which would not
/// stand out from them.
pub(crate) fn lit_words() -> Rgba {
    let accent = acc();
    if accent::has_hue(accent) {
        accent
    } else {
        ink()
    }
}

/// How each appearance wears the system's accent, and the grounds it is
/// read on. In the dark the chroma is about the most that every hue can
/// show in sRGB at that lightness, so a green keeps as much of itself as a
/// blue and no accent is louder than another.
pub(crate) const DARK_LOOK: Look = Look {
    lightness: 0.76,
    chroma: 0.12,
    toward: 1.,
    grounds: &[BG.0, BAR_BG.0, SURF.0, RAISED.0],
    tint: LIT_TINT,
};
pub(crate) const LIGHT_LOOK: Look = Look {
    lightness: 0.55,
    chroma: 0.18,
    toward: 0.,
    grounds: &[BG.1, BAR_BG.1, SURF.1, RAISED.1],
    tint: LIT_TINT,
};

/// `acc`, worked out for each appearance as the accent changes.
static ACCENT: [AtomicU32; 2] = [AtomicU32::new(ACC.0), AtomicU32::new(ACC.1)];
/// `system_accent`, worked out as the accent changes.
static SYSTEM_ACCENT: AtomicU32 = AtomicU32::new(crate::platform::CONTROL_ACCENT);

/// The system's accent as its own controls wear it: `system`'s, or the
/// system's default where it names none.
fn control_accent(system: Option<Rgba>) -> u32 {
    system.map_or(crate::platform::CONTROL_ACCENT, |system| {
        accent::hex(accent::control(system))
    })
}

/// The accent for each appearance, dark first: `system`'s, or the app's own
/// where the system names none.
fn accents(system: Option<Rgba>) -> Token {
    match system {
        Some(system) => (
            accent::hex(accent::derive(system, &DARK_LOOK)),
            accent::hex(accent::derive(system, &LIGHT_LOOK)),
        ),
        None => ACC,
    }
}

/// Follows the system's accent, `None` where it leaves the app its own;
/// whether that changed the palette. `RITORNEL_GPUI_ACCENT=#rrggbb` or
/// `none` holds one, for a developer's pictures, without changing the
/// system's.
pub(crate) fn follow_accent(system: Option<Rgba>) -> bool {
    static HELD: OnceLock<Option<Option<Rgba>>> = OnceLock::new();
    let held = HELD.get_or_init(|| {
        std::env::var("RITORNEL_GPUI_ACCENT")
            .ok()
            .and_then(|value| accent::parse(&value))
    });
    let system = (*held).unwrap_or(system);
    let (dark, light) = accents(system);
    let control = control_accent(system);
    let [dark_acc, light_acc] = &ACCENT;
    let changed = [
        dark_acc.swap(dark, Ordering::Relaxed) != dark,
        light_acc.swap(light, Ordering::Relaxed) != light,
        SYSTEM_ACCENT.swap(control, Ordering::Relaxed) != control,
    ];
    changed.contains(&true)
}

// ---- How a control answers the pointer ("Frame and navigation" in
// docs/internals/desktop.md). Under the pointer, a control steps up by what
// it is: one with no ground of its own takes `fill` as one (`hovered`), one
// that rests on `fill` takes `fill2` (`hovered_on_fill`), words or an icon
// alone turn to the ink (`hovered_ink`), and one solid with the ink lightens
// a little (`hovered_solid`). Pressed, every control dims (`pressed`), and a
// cover that rose under the pointer settles (`lifted`, `settled`). Each is a
// style, for `.hover(theme::hovered)` and `.active(theme::pressed)` or for
// more than one change at once in a closure of its own.

/// How much a control pressed keeps of itself, its ground and what is on it
/// alike: as AppKit's push button dims, and no further than keeps the
/// faintest words at 3:1 on the palette's grounds (WCAG 2's for what is not
/// text). A control on a tint of its own, as a lit toggle, dips a little
/// below that for as long as the press lasts.
const PRESSED: f32 = 0.8;
/// How much a control solid with the ink keeps of itself under the pointer.
const SOLID_HOVER: f32 = 0.88;
// A press shows past a hover.
const _: () = assert!(SOLID_HOVER > PRESSED);
/// How far a cover rises under the pointer, in points.
const LIFT: f32 = 2.;

/// A control with no ground of its own, under the pointer.
pub(crate) fn hovered(style: StyleRefinement) -> StyleRefinement {
    style.bg(fill())
}
/// A control that rests on `fill`, or in a row that does, under the pointer.
pub(crate) fn hovered_on_fill(style: StyleRefinement) -> StyleRefinement {
    style.bg(fill2())
}
/// Words or an icon alone, under the pointer.
pub(crate) fn hovered_ink(style: StyleRefinement) -> StyleRefinement {
    style.text_color(ink())
}
/// A control solid with the ink, under the pointer.
pub(crate) fn hovered_solid(style: StyleRefinement) -> StyleRefinement {
    style.opacity(SOLID_HOVER)
}
/// Any control, pressed.
pub(crate) fn pressed(style: StyleRefinement) -> StyleRefinement {
    style.opacity(PRESSED)
}
/// A cover, under the pointer: it rises.
pub(crate) fn lifted(style: StyleRefinement) -> StyleRefinement {
    style.top(px(-LIFT))
}
/// A cover, pressed: it settles back.
pub(crate) fn settled(style: StyleRefinement) -> StyleRefinement {
    style.top(px(0.))
}

/// `a` and `b` mixed, `t` of the way to `b`.
pub(crate) fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// Deterministic FNV-1a 32-bit hash over UTF-16 code units.
pub(crate) fn hash(text: &str) -> u32 {
    let mut value: u32 = 2166136261;
    for unit in text.encode_utf16() {
        value ^= u32::from(unit);
        value = value.wrapping_mul(16777619);
    }
    value
}

/// Soft colour pair (top-to-bottom) derived from a name for a tile whose
/// cover has not been seen.
pub(crate) fn soft(name: &str) -> (Hsla, Hsla) {
    let h = hash(name);
    let h1 = (h % 360) as f32;
    let h2 = ((h1 as u32 + 40 + ((h >> 9) % 80)) % 360) as f32;
    (
        hsla(h1 / 360., 0.26, 0.36, 1.),
        hsla(h2 / 360., 0.18, 0.22, 1.),
    )
}

/// A cover's average colour as a tile's ground: kept, a little deeper.
pub(crate) fn ground(color: Rgba) -> (Hsla, Hsla) {
    let top: Hsla = color.into();
    let mut bottom = top;
    bottom.l *= 0.7;
    (top, bottom)
}

/// HSL (degrees, 0 to 1, 0 to 1) as RGB, 0 to 1.
fn hsl(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c = (1. - (2. * l - 1.).abs()) * s;
    let x = c * (1. - ((h / 60.) % 2. - 1.).abs());
    let m = l - c / 2.;
    let (r, g, b) = match (h / 60.) as u32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    [r + m, g + m, b + m]
}

/// Most soft-colour pictures kept while scrolling (never evicting entries
/// drawn in the current or previous frame).
const SOFT_CAPACITY: usize = 64;
/// Byte budget for soft-colour pictures kept while scrolling.
const SOFT_BUDGET: usize = 16 * 1024 * 1024;
/// Soft-colour pictures kept at rest beyond what is still on screen.
const SOFT_REST_KEEP: usize = 16;
/// Byte budget for soft-colour pictures kept at rest.
const SOFT_REST_BUDGET: usize = 4 * 1024 * 1024;
/// Number of frame ticks an entry is protected after its last use, so calling
/// `flush_soft` from both the wall and the player bar in the same frame still
/// shields every visible tile across repaints.
const SOFT_FRAME_GUARD: u64 = 3;

/// Painted soft colours, by name and side, least recently used first out, and
/// evicted pictures waiting to be dropped from the sprite atlas.
#[derive(Default)]
struct SoftCache {
    entries:
        std::collections::HashMap<(String, u32), (std::sync::Arc<gpui::RenderImage>, usize, u64)>,
    bytes: usize,
    frame: u64,
    evicted: Vec<std::sync::Arc<gpui::RenderImage>>,
}

impl SoftCache {
    fn advance_frame(&mut self) {
        self.frame += 1;
    }

    fn get_or_insert(&mut self, name: &str, side: u32) -> std::sync::Arc<gpui::RenderImage> {
        let key = (name.to_owned(), side);
        if let Some((image, _, used)) = self.entries.get_mut(&key) {
            *used = self.frame;
            return std::sync::Arc::clone(image);
        }
        let image = std::sync::Arc::new(soft_image(name, side));
        let bytes = (side as usize)
            .saturating_mul(side as usize)
            .saturating_mul(4);
        self.bytes += bytes;
        self.entries
            .insert(key, (std::sync::Arc::clone(&image), bytes, self.frame));
        self.shrink(SOFT_CAPACITY, SOFT_BUDGET);
        image
    }

    /// Evicts the oldest entries that have not been drawn in recent frames
    /// until at most `keep` entries and `keep_bytes` remain (or only recently
    /// drawn entries are left).
    fn shrink(&mut self, keep: usize, keep_bytes: usize) -> usize {
        if self.entries.len() <= keep && self.bytes <= keep_bytes {
            return 0;
        }
        let mut order: Vec<(u64, (String, u32))> = self
            .entries
            .iter()
            .filter(|(_, (_, _, used))| used.saturating_add(SOFT_FRAME_GUARD) < self.frame)
            .map(|(key, (_, _, used))| (*used, key.clone()))
            .collect();
        order.sort_unstable_by_key(|(used, _)| *used);
        let mut removed = 0;
        for (_, key) in order {
            if self.entries.len() <= keep && self.bytes <= keep_bytes {
                break;
            }
            if let Some((image, bytes, _)) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(bytes);
                self.evicted.push(image);
                removed += 1;
            }
        }
        removed
    }

    /// Takes evicted pictures that nothing else still holds, so they can be
    /// removed from the sprite atlas without disturbing a copy still in flight.
    fn take_unshared_evicted(&mut self) -> Vec<std::sync::Arc<gpui::RenderImage>> {
        let mut ready = Vec::new();
        let mut i = 0;
        while i < self.evicted.len() {
            let unshared = self
                .evicted
                .get(i)
                .is_some_and(|image| std::sync::Arc::strong_count(image) == 1);
            if unshared {
                ready.push(self.evicted.swap_remove(i));
            } else {
                i += 1;
            }
        }
        ready
    }
}

thread_local! {
    static SOFT: std::cell::RefCell<SoftCache> = Default::default();
}

/// `soft_image`, kept for the most recently used tiles and dropped from the
/// sprite atlas when evicted (`flush_soft`).
pub(crate) fn soft_cached(name: &str, side: u32) -> std::sync::Arc<gpui::RenderImage> {
    SOFT.with(|soft| soft.borrow_mut().get_or_insert(name, side))
}

/// Whether `image` is one of the soft-colour cache's, kept or evicted: the
/// cache drops it from the sprite atlas itself.
pub(crate) fn soft_holds(image: &std::sync::Arc<gpui::RenderImage>) -> bool {
    SOFT.with(|soft| {
        let cache = soft.borrow();
        cache
            .entries
            .values()
            .any(|(held, _, _)| std::sync::Arc::ptr_eq(held, image))
            || cache
                .evicted
                .iter()
                .any(|held| std::sync::Arc::ptr_eq(held, image))
    })
}

/// Advances the soft-cache frame clock and drops any evicted soft-colour
/// pictures from every window's sprite atlas once no view or flight holds them.
pub(crate) fn flush_soft(window: &mut gpui::Window, cx: &mut gpui::App) {
    let gone = SOFT.with(|soft| {
        let mut cache = soft.borrow_mut();
        cache.advance_frame();
        if cache.evicted.is_empty() {
            return Vec::new();
        }
        cache.take_unshared_evicted()
    });
    for image in gone {
        cx.drop_image(image, Some(window));
    }
}

/// At rest, trims cached soft-colour pictures not drawn in recent frames down
/// to `SOFT_REST_KEEP` and drops evicted ones from every window's sprite atlas.
pub(crate) fn trim_soft(window: &mut gpui::Window, cx: &mut gpui::App) {
    let (removed, gone) = SOFT.with(|soft| {
        let mut cache = soft.borrow_mut();
        let removed = cache.shrink(SOFT_REST_KEEP, SOFT_REST_BUDGET);
        let gone = cache.take_unshared_evicted();
        (removed, gone)
    });
    let dropped = !gone.is_empty();
    for image in gone {
        cx.drop_image(image, Some(window));
    }
    if removed > 0 || dropped {
        crate::platform::give_back();
    }
}

/// Soft placeholder artwork for a record with no cover: two glows of
/// related hues over a dark ground, approximating two radial gradient
/// layers. GPUI has only linear gradients, so it is painted here, `side`
/// pixels square—the exact pixels it is drawn at, since GPUI's atlas has no
/// padding and an image drawn larger than its pixels bleeds its neighbours.
pub(crate) fn soft_image(name: &str, side: u32) -> gpui::RenderImage {
    let mut pixels = soft_pixels(name, side);
    // BGRA for GPUI.
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    gpui::RenderImage::new(vec![image::Frame::new(pixels)])
}

/// The same soft colour, as RGBA pixels, for blurring as a picture is.
pub(crate) fn soft_pixels(name: &str, side: u32) -> image::RgbaImage {
    let h = hash(name);
    // Shifted and divided as signed 32-bit values, so these can be negative;
    // changing that would change every placeholder colour.
    let signed = h as i32;
    let h1 = (h % 360) as f32;
    let h2 = (h1 as i32 + 40 + (signed >> 9) % 80).rem_euclid(360) as f32;
    let x = (25 + (signed >> 3) % 40) as f32 / 100.;
    let y = (25 + (signed >> 6) % 40) as f32 / 100.;
    let ground = hsl(h1, 0.12, 0.19);
    // Each glow: its centre, its colour, and its radius (`far * stop`),
    // computed once before the pixel loop.
    let radius = |(cx, cy): (f32, f32), stop: f32| {
        let far = [(0f32, 0f32), (1., 0.), (0., 1.), (1., 1.)]
            .iter()
            .map(|(fx, fy)| ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt())
            .fold(0f32, f32::max);
        far * stop
    };
    let glows = [
        (
            (1. - x, 1. - y),
            hsl(h2, 0.30, 0.40),
            radius((1. - x, 1. - y), 0.62),
        ),
        ((x, y), hsl(h1, 0.34, 0.52), radius((x, y), 0.58)),
    ];
    let inv_side = 1. / side.max(1) as f32;
    let mut pixels = image::RgbaImage::new(side, side);
    for (px, py, out) in pixels.enumerate_pixels_mut() {
        let (u, v) = ((px as f32 + 0.5) * inv_side, (py as f32 + 0.5) * inv_side);
        let mut color = ground;
        // The second layer is painted first: CSS's first layer is on top.
        for ((cx, cy), glow, reach) in glows {
            let t = (((u - cx).powi(2) + (v - cy).powi(2)).sqrt() / reach).min(1.);
            let alpha = 0.9 * (1. - t);
            for (channel, glowing) in color.iter_mut().zip(glow) {
                *channel = *channel * (1. - alpha) + glowing * alpha;
            }
        }
        let byte = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
        *out = image::Rgba([byte(color[0]), byte(color[1]), byte(color[2]), 255]);
    }
    pixels
}

/// A hue as the deep colour used to paint a banner while its picture loads,
/// independent of palette.
pub(crate) fn deep(color: Option<ritornel_contract::ArtColor>) -> Option<Rgba> {
    let color = color?;
    let saturation = f32::from(color.saturation.clamp(18, 58)) / 100.;
    let [r, g, b] = hsl(f32::from(color.hue), saturation, 0.22);
    Some(Rgba { r, g, b, a: 1. })
}

/// A hue the picture of your years borrows, at the palette's lightness
/// (`--tl`: 66% dark, 38% light).
pub(crate) fn tinted(hue: u16, saturation: u8) -> Rgba {
    let lightness = if dark() { 0.66 } else { 0.38 };
    let [r, g, b] = hsl(f32::from(hue), f32::from(saturation) / 100., lightness);
    Rgba { r, g, b, a: 1. }
}

/// A box shadow as CSS writes it, `0 <y> <blur> <color>`. CSS blurs a
/// shadow by a Gaussian whose deviation is half its blur radius (CSS
/// Backgrounds and Borders, "Drop Shadows"); GPUI's blur radius is that
/// deviation itself (`sigma` in its shaders), so CSS's number is halved.
pub(crate) fn css_shadow(y: f32, blur: f32, color: impl Into<gpui::Hsla>) -> gpui::BoxShadow {
    gpui::BoxShadow::new(gpui::px(0.), gpui::px(y), color.into()).blur_radius(gpui::px(blur / 2.))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pressed_control_s_words_still_show_on_the_palette_s_grounds() {
        for dark in [true, false] {
            for words in [INK, DIM, FAINT] {
                for ground in [BG, BAR_BG, SURF, RAISED] {
                    let ground = side(ground, dark);
                    let pressed = mix(ground, side(words, dark), PRESSED);
                    let ratio = accent::contrast(pressed, ground);
                    assert!(ratio >= 3., "dark {dark}: {words:08x?} pressed: {ratio:.2}");
                }
            }
        }
    }

    /// A token's colour in one appearance.
    fn side(token: Token, dark: bool) -> Rgba {
        rgba(if dark { token.0 } else { token.1 })
    }

    #[test]
    fn a_status_s_mark_shows_on_the_page_and_its_words_read_on_every_ground() {
        for dark in [true, false] {
            for mark in [OK, NEED, FAIL] {
                for ground in [BG, RAISED] {
                    let ratio = accent::contrast(side(mark, dark), side(ground, dark));
                    assert!(ratio >= 3., "dark {dark}: {mark:08x?}: {ratio:.2}");
                }
            }
            for words in [OK_TEXT, NEED_TEXT, FAIL_TEXT] {
                for ground in [BG, BAR_BG, SURF, RAISED] {
                    let ratio = accent::contrast(side(words, dark), side(ground, dark));
                    assert!(ratio >= 4.5, "dark {dark}: {words:08x?}: {ratio:.2}");
                }
                // And on a neutral button on the page.
                let page = side(BG, dark);
                let button = mix(
                    page,
                    Rgba {
                        a: 1.,
                        ..side(FILL, dark)
                    },
                    side(FILL, dark).a,
                );
                let ratio = accent::contrast(side(words, dark), button);
                assert!(
                    ratio >= 4.5,
                    "dark {dark}: {words:08x?} on fill: {ratio:.2}"
                );
            }
        }
    }

    #[test]
    fn red_words_and_marks_read_on_every_ground_at_rest_and_under_the_pointer() {
        // A destructive button, and a place whose output failed in the Play
        // on picker, which lies on `raised`.
        for dark in [true, false] {
            for ground in [BG, BAR_BG, SURF, RAISED] {
                let ground = side(ground, dark);
                let hovered = mix(ground, side(FAIL, dark), FAIL_HOVER);
                for (state, under) in [("rest", ground), ("hover", hovered)] {
                    let words = accent::contrast(side(FAIL_TEXT, dark), under);
                    assert!(words >= 4.5, "dark {dark}, {state}: words {words:.2}");
                    let mark = accent::contrast(side(FAIL, dark), under);
                    assert!(mark >= 3., "dark {dark}, {state}: mark {mark:.2}");
                }
            }
        }
    }

    #[test]
    fn a_place_out_of_reach_reads_in_the_picker() {
        // It cannot be chosen, so neither the pointer nor the keys light
        // it: its ring and line lie on the picker's `raised` alone.
        for dark in [true, false] {
            let ground = side(RAISED, dark);
            let mark = accent::contrast(side(NEED, dark), ground);
            assert!(mark >= 3., "dark {dark}: mark {mark:.2}");
            let words = accent::contrast(side(NEED_TEXT, dark), ground);
            assert!(words >= 4.5, "dark {dark}: words {words:.2}");
        }
    }

    #[test]
    fn a_primary_button_s_words_read_on_the_accent_at_rest_and_under_the_pointer() {
        let mut both = vec![ACC];
        for system in [0x007affff, 0xffc600ff, 0xff3b30ff, 0x28cd41ff, 0x8c8c8cff] {
            both.push(accents(Some(rgba(system))));
        }
        for accent in both {
            for dark in [true, false] {
                let (ground, words) = (side(accent, dark), side(ON_ACCENT, dark));
                let hovered = mix(ground, side(INK, dark), PRIMARY_HOVER);
                for (state, under) in [("rest", ground), ("hover", hovered)] {
                    let ratio = accent::contrast(words, under);
                    assert!(
                        ratio >= 4.5,
                        "{accent:08x?} dark {dark}, {state}: {ratio:.2}"
                    );
                }
            }
        }
    }

    #[test]
    fn amber_and_red_differ_in_lightness_where_a_badge_has_no_words() {
        for dark in [true, false] {
            let ratio = accent::contrast(side(NEED, dark), side(FAIL, dark));
            assert!(ratio >= 1.4, "dark {dark}: {ratio:.2}");
        }
    }

    #[test]
    fn working_keeps_the_app_s_own_colour_whatever_the_system_s_accent() {
        for system in [0xff3b30ff, 0x28cd41ff] {
            let (dark, light) = accents(Some(rgba(system)));
            assert_ne!((dark, light), WORKING, "{system:08x}");
        }
        assert_eq!(WORKING, ACC);
    }

    #[test]
    fn the_settings_controls_wear_the_system_s_accent_or_its_default() {
        assert_eq!(control_accent(None), crate::platform::CONTROL_ACCENT);
        // The default keeps a white knob at 3:1 on it, as a named accent is
        // darkened to.
        let knob = rgba(0xffffffff);
        let ratio = accent::contrast(rgba(crate::platform::CONTROL_ACCENT), knob);
        assert!(ratio >= accent::CONTROL_CONTRAST, "{ratio:.2}");
        for system in [0xffc600ff, 0x8c8c8cff, 0x007affff] {
            let control = control_accent(Some(rgba(system)));
            assert_eq!(control, accent::hex(accent::control(rgba(system))));
            let ratio = accent::contrast(rgba(control), knob);
            assert!(
                ratio >= accent::CONTROL_CONTRAST,
                "{system:08x}: {ratio:.2}"
            );
        }
    }

    #[test]
    fn hover_and_press_are_the_palette_s_own() {
        let style =
            |refine: fn(StyleRefinement) -> StyleRefinement| refine(StyleRefinement::default());
        assert_eq!(style(pressed).opacity, Some(PRESSED));
        assert_eq!(style(hovered_solid).opacity, Some(SOLID_HOVER));
        assert_eq!(style(lifted).inset.top, Some(px(-LIFT).into()));
        assert_eq!(style(settled).inset.top, Some(px(0.).into()));
    }

    #[test]
    fn with_no_system_accent_the_palette_keeps_its_own() {
        assert_eq!(accents(None), ACC);
        // And its own has a hue for what is lit to wear.
        assert!(accent::has_hue(rgba(ACC.0)) && accent::has_hue(rgba(ACC.1)));
    }

    #[test]
    fn a_system_accent_reads_on_every_ground_of_its_appearance() {
        for system in [0xffc600ff, 0x007affff, 0xffffffff, 0x101010ff] {
            let (dark, light) = accents(Some(rgba(system)));
            for (accent, look) in [(dark, DARK_LOOK), (light, LIGHT_LOOK)] {
                for ground in look.grounds {
                    let ratio = accent::contrast(rgba(accent), rgba(*ground));
                    assert!(ratio >= accent::CONTRAST, "{system:08x}: {ratio:.2}");
                    let lit = mix(rgba(*ground), rgba(accent), LIT_TINT);
                    let ratio = accent::contrast(rgba(accent), lit);
                    assert!(ratio >= accent::CONTRAST, "{system:08x} lit: {ratio:.2}");
                }
            }
        }
    }

    #[test]
    fn soft_cache_queues_evicted_images_until_unshared() {
        let mut cache = SoftCache::default();
        let held = cache.get_or_insert("held", 8);
        // Move past the frame guard so `held` becomes eligible for eviction.
        for _ in 0..=SOFT_FRAME_GUARD {
            cache.advance_frame();
        }
        for i in 0..SOFT_CAPACITY {
            let _ = cache.get_or_insert(&format!("item-{i}"), 8);
        }
        assert_eq!(cache.entries.len(), SOFT_CAPACITY);
        assert_eq!(cache.evicted.len(), 1);
        // Still held outside the cache, so not ready to drop from the atlas yet.
        assert!(cache.take_unshared_evicted().is_empty());
        drop(held);
        assert_eq!(cache.take_unshared_evicted().len(), 1);
        assert!(cache.evicted.is_empty());
    }

    #[test]
    fn a_soft_picture_is_known_as_the_caches_kept_or_evicted() {
        let kept = soft_cached("known-kept", 8);
        assert!(soft_holds(&kept));
        let own = std::sync::Arc::new(soft_image("known-kept", 8));
        assert!(!soft_holds(&own));
        // Evicted, it is still the cache's to let go.
        SOFT.with(|soft| {
            let mut cache = soft.borrow_mut();
            let key = ("known-kept".to_owned(), 8);
            if let Some((image, bytes, _)) = cache.entries.remove(&key) {
                cache.bytes = cache.bytes.saturating_sub(bytes);
                cache.evicted.push(image);
            }
        });
        assert!(soft_holds(&kept));
    }

    #[test]
    fn soft_cache_evicts_by_byte_budget() {
        let mut cache = SoftCache::default();
        // 512x512 RGBA = 1 MiB each; inserting 20 across aged frames must trim
        // down to SOFT_BUDGET (16 MiB) even though count < SOFT_CAPACITY (64).
        for i in 0..20 {
            let _ = cache.get_or_insert(&format!("big-{i}"), 512);
            for _ in 0..=SOFT_FRAME_GUARD {
                cache.advance_frame();
            }
        }
        assert!(cache.bytes <= SOFT_BUDGET);
        assert!(cache.entries.len() <= 16);
        assert!(!cache.evicted.is_empty());
    }

    #[test]
    fn soft_cache_protects_visible_tiles_across_rest_trim_and_wide_frames() {
        let mut cache = SoftCache::default();
        // Even more than SOFT_CAPACITY tiles drawn in one frame stay cached
        // without thrashing within the frame.
        let wide_count = SOFT_CAPACITY + 10;
        let ids: Vec<_> = (0..wide_count)
            .map(|i| cache.get_or_insert(&format!("visible-{i}"), 8).id)
            .collect();
        assert_eq!(cache.entries.len(), wide_count);
        assert!(cache.evicted.is_empty());

        // Simulate stationary repaints (wall + player bar flushing each frame)
        // with 30 visible soft tiles (> SOFT_REST_KEEP). Trimming before the
        // wall looks up its tiles must not evict or regenerate any of them.
        for _ in 0..3 {
            cache.advance_frame(); // wall flush
            let _ = cache.shrink(SOFT_REST_KEEP, SOFT_REST_BUDGET);
            for (i, expected_id) in ids.iter().enumerate().take(30) {
                let got = cache.get_or_insert(&format!("visible-{i}"), 8);
                assert_eq!(got.id, *expected_id);
            }
            cache.advance_frame(); // player bar flush
        }

        // Once off-screen tiles age past the frame guard, trimming evicts them
        // while keeping the 30 visible tiles intact.
        for _ in 0..=SOFT_FRAME_GUARD {
            cache.advance_frame();
        }
        // Touch the 30 visible tiles on the current frame, then trim.
        for (i, expected_id) in ids.iter().enumerate().take(30) {
            let got = cache.get_or_insert(&format!("visible-{i}"), 8);
            assert_eq!(got.id, *expected_id);
        }
        cache.advance_frame();
        let _ = cache.shrink(SOFT_REST_KEEP, SOFT_REST_BUDGET);
        assert_eq!(cache.entries.len(), 30);
        assert_eq!(cache.take_unshared_evicted().len(), wide_count - 30);
    }

    #[test]
    fn soft_cache_keeps_recently_used_entries_when_trimmed() {
        let mut cache = SoftCache::default();
        for i in 0..32 {
            let _ = cache.get_or_insert(&format!("item-{i}"), 8);
        }
        cache.advance_frame();
        // Touch item-0 on a later frame so it is newer than item-1..31.
        let kept = cache.get_or_insert("item-0", 8);
        for _ in 0..=SOFT_FRAME_GUARD {
            cache.advance_frame();
        }
        let removed = cache.shrink(SOFT_REST_KEEP, SOFT_REST_BUDGET);
        assert_eq!(removed, 32 - SOFT_REST_KEEP);
        assert_eq!(cache.entries.len(), SOFT_REST_KEEP);
        assert!(cache.entries.contains_key(&("item-0".to_owned(), 8)));
        drop(kept);
        assert_eq!(cache.take_unshared_evicted().len(), removed);
    }
}
