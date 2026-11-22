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

/// A JPEG cover, blurred, for a banner: drawn square over the banner 30% of
/// the way down (`background-position: center 30%`), it is cut square the
/// same way first, so that a picture that is not square is not squashed.
pub(crate) fn jpeg(bytes: &[u8], radius: usize, saturation: f32) -> Option<RenderImage> {
    blurred(bytes, 0.3, radius, saturation, 1.)
}

/// A JPEG cover, blurred, saturated and brightened, for the window's wash or
/// Lean back: drawn square over the window's middle, it is cut square at
/// its middle first.
pub(crate) fn jpeg_toned(
    bytes: &[u8],
    radius: usize,
    saturation: f32,
    brightness: f32,
) -> Option<RenderImage> {
    blurred(bytes, 0.5, radius, saturation, brightness)
}

fn blurred(
    bytes: &[u8],
    down: f32,
    radius: usize,
    saturation: f32,
    brightness: f32,
) -> Option<RenderImage> {
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;
    let rgba = crate::covers::squared(decoded, down)
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

    /// A JPEG of three bands down it, green, red and blue, each a third.
    fn banded(w: u32, h: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(w, h, |_, y| match 3 * y / h {
            0 => image::Rgb([0, 255, 0]),
            1 => image::Rgb([255, 0, 0]),
            _ => image::Rgb([0, 0, 255]),
        });
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(&image)
            .unwrap_or_default();
        bytes
    }

    #[test]
    fn a_tall_picture_is_cut_square_before_it_is_blurred() {
        // Blurred hardly at all: a banner's is cut 30% down, rows 60 to 160
        // of 300, so green at its top; the wash's at the middle, rows 100
        // to 200, red throughout. Squashed, both would start green and end
        // blue.
        let top = |image: Option<RenderImage>| -> Option<[u8; 4]> {
            let image = image?;
            let bytes = image.as_bytes(0)?;
            let at = (2 * SIDE as usize + SIDE as usize / 2) * 4;
            bytes.get(at..at + 4)?.try_into().ok()
        };
        let foot = |image: Option<RenderImage>| -> Option<[u8; 4]> {
            let image = image?;
            let bytes = image.as_bytes(0)?;
            let at = ((SIDE as usize - 3) * SIDE as usize + SIDE as usize / 2) * 4;
            bytes.get(at..at + 4)?.try_into().ok()
        };
        let bytes = banded(100, 300);
        // BGRA.
        let banner = top(jpeg(&bytes, 0, 1.));
        assert!(
            matches!(banner, Some([_, g, r, _]) if g > 200 && r < 80),
            "{banner:?}"
        );
        let wash = top(jpeg_toned(&bytes, 0, 1., 1.));
        assert!(
            matches!(wash, Some([_, g, r, _]) if r > 200 && g < 80),
            "{wash:?}"
        );
        let wash = foot(jpeg_toned(&bytes, 0, 1., 1.));
        assert!(
            matches!(wash, Some([b, _, r, _]) if r > 200 && b < 80),
            "{wash:?}"
        );
    }

    #[test]
    fn the_radius_follows_how_large_the_picture_is_drawn() {
        // The banner across a window of 1,400: σ 3.7 pixels at 128.
        assert_eq!(radius(44., 1520.), 3);
        // The wash over one of 1,400 by 900: σ 5.2.
        assert_eq!(radius(64., 1580.), 5);
        assert!(radius(44., 800.) > radius(44., 2400.));
    }
}
