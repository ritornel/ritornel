//! A picture blurred out of all detail, as the real client's CSS blurs one
//! (`filter: blur(σ) saturate(s)`): GPUI has no blur for an image, so it is
//! made on the CPU from the picture at 128 pixels, and drawn stretched.
//! Three box passes are close to a Gaussian; their radius is worked out from
//! the CSS σ and how large the picture is drawn, so that the blur on screen
//! is the real one's.
//!
//! The banner's is `blur(44px) saturate(1.3)` over the banner's width and
//! 120 points; the wash's `blur(64px) saturate(1.5)` over the window and 180,
//! as is Lean back's in the dark palette, which in the light one is
//! `blur(64px) saturate(1.25) brightness(1.18)`.

use gpui::RenderImage;

/// The side the picture is blurred at.
pub(crate) const SIDE: u32 = 128;

/// The banner's blur and saturation (`parts/Banner.svelte`).
pub(crate) const BANNER: (f32, f32) = (44., 1.3);
/// The wash's (`frame/Wash.svelte`).
pub(crate) const WASH: (f32, f32) = (64., 1.5);

/// The box radius, in pixels of the picture at `SIDE`, whose three passes
/// blur as CSS's `blur(sigma)` does on a picture drawn `drawn` points wide.
pub(crate) fn radius(sigma: f32, drawn: f32) -> usize {
    let sigma = sigma * SIDE as f32 / drawn.max(1.);
    // Three passes of radius r: σ² = ((2r + 1)² − 1) / 4.
    (((4. * sigma * sigma + 1.).sqrt() - 1.) / 2.)
        .round()
        .max(1.) as usize
}

/// The wash's and Lean back's saturation and brightness in each palette:
/// in the light one their filter is `blur(64px) saturate(1.25)
/// brightness(1.18)` (`frame/Wash.svelte`, `frame/LeanBack.svelte`).
pub(crate) fn wash_tone(dark: bool) -> (f32, f32) {
    if dark { (WASH.1, 1.) } else { (1.25, 1.18) }
}

/// A JPEG cover, blurred.
pub(crate) fn jpeg(bytes: &[u8], radius: usize, saturation: f32) -> Option<RenderImage> {
    jpeg_toned(bytes, radius, saturation, 1.)
}

/// A JPEG cover, blurred, saturated and brightened.
pub(crate) fn jpeg_toned(
    bytes: &[u8],
    radius: usize,
    saturation: f32,
    brightness: f32,
) -> Option<RenderImage> {
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;
    let rgba = decoded
        .resize_exact(SIDE, SIDE, image::imageops::FilterType::Triangle)
        .into_rgba8();
    Some(toned(&rgba, radius, saturation, brightness))
}

/// An RGBA picture, blurred and saturated, as BGRA for GPUI.
pub(crate) fn pixels(rgba: &image::RgbaImage, radius: usize, saturation: f32) -> RenderImage {
    toned(rgba, radius, saturation, 1.)
}

/// An RGBA picture, blurred, saturated and then brightened, in CSS's order
/// (`blur() saturate() brightness()`), as BGRA for GPUI.
pub(crate) fn toned(
    rgba: &image::RgbaImage,
    radius: usize,
    saturation: f32,
    brightness: f32,
) -> RenderImage {
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let mut buffer: Vec<[f32; 3]> = rgba
        .pixels()
        .map(|p| [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])])
        .collect();
    for _ in 0..3 {
        buffer = box_blur(&buffer, w, h, radius, true);
        buffer = box_blur(&buffer, w, h, radius, false);
    }
    let mut out = image::RgbaImage::new(rgba.width(), rgba.height());
    for (pixel, target) in buffer.iter().zip(out.pixels_mut()) {
        let grey = 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2];
        let saturate = |value: f32| {
            ((grey + (value - grey) * saturation).clamp(0., 255.) * brightness).clamp(0., 255.)
                as u8
        };
        *target = image::Rgba([
            saturate(pixel[2]),
            saturate(pixel[1]),
            saturate(pixel[0]),
            255,
        ]);
    }
    RenderImage::new(vec![image::Frame::new(out)])
}

fn box_blur(source: &[[f32; 3]], w: usize, h: usize, radius: usize, across: bool) -> Vec<[f32; 3]> {
    let mut out = vec![[0.; 3]; source.len()];
    let (lines, length) = if across { (h, w) } else { (w, h) };
    let at = |line: usize, i: usize| if across { line * w + i } else { i * w + line };
    let span = (2 * radius + 1) as f32;
    for line in 0..lines {
        // Edges repeat, so the colour does not darken towards them.
        let pick = |i: isize| {
            source
                .get(at(line, i.clamp(0, length as isize - 1) as usize))
                .copied()
                .unwrap_or_default()
        };
        let mut sum = [0f32; 3];
        for i in -(radius as isize)..=(radius as isize) {
            let p = pick(i);
            sum = std::array::from_fn(|c| {
                sum.get(c).copied().unwrap_or(0.) + p.get(c).copied().unwrap_or(0.)
            });
        }
        for i in 0..length {
            if let Some(cell) = out.get_mut(at(line, i)) {
                *cell = sum.map(|channel| channel / span);
            }
            let (add, remove) = (
                pick(i as isize + radius as isize + 1),
                pick(i as isize - radius as isize),
            );
            sum = std::array::from_fn(|c| {
                sum.get(c).copied().unwrap_or(0.) + add.get(c).copied().unwrap_or(0.)
                    - remove.get(c).copied().unwrap_or(0.)
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_radius_follows_how_large_the_picture_is_drawn() {
        // The banner across a window of 1,400: σ 3.7 pixels at 128.
        assert_eq!(radius(44., 1520.), 3);
        // The wash over one of 1,400 by 900: σ 5.2.
        assert_eq!(radius(64., 1580.), 5);
        assert!(radius(44., 800.) > radius(44., 2400.));
    }
}
