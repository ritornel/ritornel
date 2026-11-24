//! The desktop palettes ("Frame and navigation" in
//! docs/internals/desktop.md): Graphite (dark) and Mist (light). The window
//! follows the system's appearance; there is no switch.

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
/// What went wrong: `--warn`.
pub(crate) fn warn() -> Rgba {
    pick(0xe59a6cff, 0xb4572aff)
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
