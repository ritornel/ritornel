//! Covers for invented records: a gradient and a few plain shapes in
//! colours drawn from a seed, written as a JPEG at the size real covers
//! come in, so that decoding and scaling them costs what it would.

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageEncoder};

use super::Rng;

/// A colour, as three channels from 0 to 1.
#[derive(Debug, Clone, Copy)]
struct Colour([f32; 3]);

impl Colour {
    /// A colour from hue (degrees), saturation and lightness (0 to 1).
    fn hsl(hue: f32, saturation: f32, lightness: f32) -> Self {
        let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
        let segment = (hue.rem_euclid(360.0)) / 60.0;
        let x = chroma * (1.0 - (segment % 2.0 - 1.0).abs());
        let (r, g, b) = match segment as u32 {
            0 => (chroma, x, 0.0),
            1 => (x, chroma, 0.0),
            2 => (0.0, chroma, x),
            3 => (0.0, x, chroma),
            4 => (x, 0.0, chroma),
            _ => (chroma, 0.0, x),
        };
        let m = lightness - chroma / 2.0;
        Self([r + m, g + m, b + m])
    }

    fn mix(self, other: Self, amount: f32) -> Self {
        let [a, b, c] = self.0;
        let [d, e, f] = other.0;
        Self([
            a + (d - a) * amount,
            b + (e - b) * amount,
            c + (f - c) * amount,
        ])
    }
}

/// One shape laid over the gradient.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// A disc: centre and radius, as fractions of the side.
    Disc { x: f32, y: f32, r: f32 },
    /// A ring: centre, radius and width.
    Ring { x: f32, y: f32, r: f32, w: f32 },
    /// A band across the cover at an angle: its offset and width.
    Band { angle: f32, at: f32, w: f32 },
    /// A rectangle: its corners.
    Block { x0: f32, y0: f32, x1: f32, y1: f32 },
}

impl Shape {
    fn covers(self, x: f32, y: f32) -> bool {
        match self {
            Self::Disc { x: cx, y: cy, r } => (x - cx).powi(2) + (y - cy).powi(2) <= r * r,
            Self::Ring { x: cx, y: cy, r, w } => {
                let distance = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
                (distance - r).abs() <= w / 2.0
            }
            Self::Band { angle, at, w } => {
                let along = x * angle.cos() + y * angle.sin();
                (along - at).abs() <= w / 2.0
            }
            Self::Block { x0, y0, x1, y1 } => x >= x0 && x <= x1 && y >= y0 && y <= y1,
        }
    }
}

/// A cover, `side` pixels square, painted from `seed`, as JPEG bytes.
pub(super) fn cover(seed: u64, side: u32) -> Result<Vec<u8>, String> {
    let mut rng = Rng::new(seed);
    let hue = rng.float() * 360.0;
    let spread = 30.0 + rng.float() * 150.0;
    let dark = rng.float() < 0.35;
    let (l0, l1) = if dark { (0.12, 0.38) } else { (0.35, 0.72) };
    let from = Colour::hsl(hue, 0.35 + rng.float() * 0.5, l0 + rng.float() * 0.1);
    let to = Colour::hsl(
        hue + spread,
        0.3 + rng.float() * 0.5,
        l1 + rng.float() * 0.1,
    );
    let angle = rng.float() * std::f32::consts::TAU;
    let (dx, dy) = (angle.cos(), angle.sin());

    let mut shapes = Vec::new();
    for _ in 0..rng.below(5) {
        let shape = match rng.below(4) {
            0 => Shape::Disc {
                x: rng.float(),
                y: rng.float(),
                r: 0.08 + rng.float() * 0.3,
            },
            1 => Shape::Ring {
                x: rng.float(),
                y: rng.float(),
                r: 0.1 + rng.float() * 0.35,
                w: 0.01 + rng.float() * 0.05,
            },
            2 => Shape::Band {
                angle: rng.float() * std::f32::consts::PI,
                at: rng.float() * 1.4 - 0.2,
                w: 0.02 + rng.float() * 0.2,
            },
            _ => {
                let (x, y) = (rng.float() * 0.8, rng.float() * 0.8);
                Shape::Block {
                    x0: x,
                    y0: y,
                    x1: x + 0.1 + rng.float() * 0.4,
                    y1: y + 0.1 + rng.float() * 0.4,
                }
            }
        };
        let ink = Colour::hsl(
            hue + rng.float() * 360.0,
            0.2 + rng.float() * 0.6,
            if dark { 0.55 } else { 0.18 } + rng.float() * 0.25,
        );
        shapes.push((shape, ink, 0.45 + rng.float() * 0.5));
    }

    let side_f = side as f32;
    let mut pixels = Vec::with_capacity(side as usize * side as usize * 3);
    for row in 0..side {
        let y = row as f32 / side_f;
        for column in 0..side {
            let x = column as f32 / side_f;
            let along = ((x - 0.5) * dx + (y - 0.5) * dy + 0.71) / 1.42;
            let mut colour = from.mix(to, along.clamp(0.0, 1.0));
            for (shape, ink, alpha) in &shapes {
                if shape.covers(x, y) {
                    colour = colour.mix(*ink, *alpha);
                }
            }
            for channel in colour.0 {
                pixels.push((channel.clamp(0.0, 1.0) * 255.0) as u8);
            }
        }
    }
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 86)
        .write_image(&pixels, side, side, ExtendedColorType::Rgb8)
        .map_err(|error| format!("a cover could not be written: {error}"))?;
    Ok(jpeg)
}
