//! The real client's palettes (`apps/desktop-tauri/src/app.css`): Graphite, the
//! prototype's default, dark; and Mist, light. The window follows the
//! system's appearance, as the real app does; there is no switch.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Hsla, Rgba, WindowAppearance, hsla, rgba};

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

/// The token for this appearance: dark first, light second.
fn pick(dark: u32, light: u32) -> Rgba {
    if self::dark() {
        rgba(dark)
    } else {
        rgba(light)
    }
}

pub(crate) fn bg() -> Rgba {
    pick(0x1b1c1fff, 0xf7f8faff)
}
pub(crate) fn bar_bg() -> Rgba {
    pick(0x202125ff, 0xf1f3f6ff)
}
/// A surface over the page, as Up next's card is.
pub(crate) fn surf() -> Rgba {
    pick(0x232428ff, 0xedeff3ff)
}
pub(crate) fn raised() -> Rgba {
    pick(0x2d2e33ff, 0xffffffff)
}
pub(crate) fn fill() -> Rgba {
    pick(0xffffff12, 0x1c22300f)
}
pub(crate) fn fill2() -> Rgba {
    pick(0xffffff26, 0x1c223024)
}
pub(crate) fn line() -> Rgba {
    pick(0xffffff17, 0x1c22301a)
}
pub(crate) fn ink() -> Rgba {
    pick(0xe9e9ecff, 0x1c2230ff)
}
pub(crate) fn dim() -> Rgba {
    pick(0xa8a9b0ff, 0x5a6273ff)
}
pub(crate) fn faint() -> Rgba {
    pick(0x8d8e97ff, 0x616979ff)
}
pub(crate) fn acc() -> Rgba {
    pick(0x8fb4d9ff, 0x5a61d6ff)
}
/// Under a large cover: `--cover-shadow`.
pub(crate) fn cover_shadow() -> Rgba {
    pick(0x00000061, 0x28261e38)
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

/// A number from a string, the same every time (the real client's `soft`).
pub(crate) fn hash(text: &str) -> u32 {
    let mut value: u32 = 2166136261;
    for unit in text.encode_utf16() {
        value ^= u32::from(unit);
        value = value.wrapping_mul(16777619);
    }
    value
}

/// Soft colour from a name, for a tile whose cover has not been seen: the
/// two hues of the real client's `soft()`, as a top-to-bottom pair.
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

/// Painted soft colours, by name and side, and the order they were made in.
type SoftCache = (
    std::collections::HashMap<(String, u32), std::sync::Arc<gpui::RenderImage>>,
    std::collections::VecDeque<(String, u32)>,
);

thread_local! {
    static SOFT: std::cell::RefCell<SoftCache> = Default::default();
}

/// `soft_image`, kept for the last 160 asked for (a few screens of tiles).
pub(crate) fn soft_cached(name: &str, side: u32) -> std::sync::Arc<gpui::RenderImage> {
    SOFT.with(|soft| {
        let (map, order) = &mut *soft.borrow_mut();
        let key = (name.to_owned(), side);
        if let Some(image) = map.get(&key) {
            return std::sync::Arc::clone(image);
        }
        let image = std::sync::Arc::new(soft_image(name, side));
        map.insert(key.clone(), std::sync::Arc::clone(&image));
        order.push_back(key);
        if order.len() > 160
            && let Some(old) = order.pop_front()
        {
            map.remove(&old);
        }
        image
    })
}

/// The real client's `soft()` for a record with no cover, drawn: two glows
/// of related hues over a dark ground, as CSS draws its two
/// `radial-gradient(circle at …)` layers. GPUI has only linear gradients,
/// so it is painted here, `side` pixels square: the pixels it is drawn at,
/// since GPUI's atlas has no padding and an image drawn larger than its
/// pixels takes in its neighbours' along its edges.
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
    // JavaScript's `>>` is a signed shift of the 32 bits, and its `%` keeps
    // the sign, so these can be negative, as they are in the real client.
    let signed = h as i32;
    let h1 = (h % 360) as f32;
    let h2 = (h1 as i32 + 40 + (signed >> 9) % 80).rem_euclid(360) as f32;
    let x = (25 + (signed >> 3) % 40) as f32 / 100.;
    let y = (25 + (signed >> 6) % 40) as f32 / 100.;
    let ground = hsl(h1, 0.12, 0.19);
    // Each glow: its centre, its colour, and where it has gone, as a share
    // of the distance from the centre to the farthest corner.
    let glows = [
        ((1. - x, 1. - y), hsl(h2, 0.30, 0.40), 0.62),
        ((x, y), hsl(h1, 0.34, 0.52), 0.58),
    ];
    let mut pixels = image::RgbaImage::new(side, side);
    for (px, py, out) in pixels.enumerate_pixels_mut() {
        let (u, v) = (
            (px as f32 + 0.5) / side as f32,
            (py as f32 + 0.5) / side as f32,
        );
        let mut color = ground;
        // The second layer is painted first: CSS's first layer is on top.
        for ((cx, cy), glow, stop) in glows {
            let far = [(0f32, 0f32), (1., 0.), (0., 1.), (1., 1.)]
                .iter()
                .map(|(fx, fy)| ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt())
                .fold(0f32, f32::max);
            let t = (((u - cx).powi(2) + (v - cy).powi(2)).sqrt() / (far * stop)).min(1.);
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

/// A hue as the deep colour the real client paints a banner with while
/// its picture loads (`wash.svelte.ts`, `deep`): whatever the palette.
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
