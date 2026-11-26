//! The accent: the colour the system names (`platform::accents`), made the
//! app's for each appearance ("Frame and navigation" in
//! docs/internals/desktop.md).
//!
//! The accent marks what is lit: a star, the seek bar held, the song
//! playing, the tab open, a switch's label. It is read as text and lines
//! on the app's grounds, so it keeps the system's hue and as much of its
//! chroma as the appearance allows. Its lightness is the palette's own,
//! and it is moved away from the grounds until it reads at `CONTRAST` on
//! each. A grey accent, as a Mac's Graphite, stays grey. The work is done
//! in Oklab, where lightness and chroma are what the eye sees, and
//! contrast is WCAG 2's.

use gpui::{Rgba, rgba};

/// The contrast an accent keeps with every ground it is drawn on: WCAG 2's
/// for text.
pub(crate) const CONTRAST: f32 = 4.5;

/// The contrast a control's accent keeps with the white knob or mark drawn
/// on it: WCAG 2's for what is not text.
pub(crate) const CONTROL_CONTRAST: f32 = 3.;

/// How an appearance wears an accent.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Look {
    /// Oklab's lightness the accent starts from.
    pub lightness: f32,
    /// The most chroma it keeps.
    pub chroma: f32,
    /// Where its lightness moves to read on the grounds: 1 away from dark
    /// grounds, 0 away from light ones.
    pub toward: f32,
    /// What it is drawn on, as `0xrrggbbaa`.
    pub grounds: &'static [u32],
    /// The share of the accent in its own tint of a ground, which it is
    /// drawn on too, as on a lit button of the player bar.
    pub tint: f32,
}

/// Steps from a colour's lightness to `toward`, the last of which reads on
/// any ground.
const STEPS: u16 = 64;

/// The system's `accent` as `look` wears it.
pub(crate) fn derive(accent: Rgba, look: &Look) -> Rgba {
    let [_, chroma, hue] = oklch(accent);
    let start = shown(look.lightness, chroma.min(look.chroma), hue);
    readable(start, look.toward, |color| {
        look.grounds.iter().all(|ground| {
            let ground = rgba(*ground);
            contrast(color, ground) >= CONTRAST
                && contrast(color, over(ground, color, look.tint)) >= CONTRAST
        })
    })
}

/// The system's `accent` as a control drawn like the system's wears it:
/// as it is, or darker where a white knob would not show on it.
pub(crate) fn control(accent: Rgba) -> Rgba {
    readable(Rgba { a: 1., ..accent }, 0., |color| {
        contrast(color, rgba(0xffffffff)) >= CONTROL_CONTRAST
    })
}

/// The chroma under which a colour reads as a grey, as a Mac's Graphite
/// does and GNOME's slate.
const GREY: f32 = 0.04;

/// Whether `color` has a hue to be told by, beside words in greys.
pub(crate) fn has_hue(color: Rgba) -> bool {
    let [_, chroma, _] = oklch(color);
    chroma >= GREY
}

/// `color`, its lightness moved step by step toward `toward` until `reads`
/// holds of it as it is drawn, in whole bytes.
fn readable(color: Rgba, toward: f32, reads: impl Fn(Rgba) -> bool) -> Rgba {
    let [lightness, chroma, hue] = oklch(color);
    let drawn = |color: Rgba| rgba(hex(color));
    (0..=STEPS)
        .map(|step| {
            let t = f32::from(step) / f32::from(STEPS);
            lightness + (toward - lightness) * t
        })
        .map(|lightness| drawn(shown(lightness, chroma, hue)))
        .find(|color| reads(*color))
        // The last step is white or black, which reads on every ground
        // the palette has; this is only for grounds none can read on.
        .unwrap_or_else(|| drawn(shown(toward, 0., 0.)))
}

/// `color` laid over `ground` at `share`, both opaque.
fn over(ground: Rgba, color: Rgba, share: f32) -> Rgba {
    let mix = |a: f32, b: f32| a + (b - a) * share;
    Rgba {
        r: mix(ground.r, color.r),
        g: mix(ground.g, color.g),
        b: mix(ground.b, color.b),
        a: 1.,
    }
}

/// WCAG 2's contrast ratio between two opaque colours, 1 to 21.
pub(crate) fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// WCAG 2's relative luminance.
fn luminance(color: Rgba) -> f32 {
    let [r, g, b] = [color.r, color.g, color.b].map(linear);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// An sRGB channel, 0 to 1, as light.
fn linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// Light as an sRGB channel.
fn encoded(channel: f32) -> f32 {
    if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1. / 2.4) - 0.055
    }
}

/// Oklab's lightness, chroma, and hue in radians.
fn oklch(color: Rgba) -> [f32; 3] {
    let [r, g, b] = [color.r, color.g, color.b].map(|c| linear(c.clamp(0., 1.)));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let lightness = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let b = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    [lightness, a.hypot(b), b.atan2(a)]
}

/// Linear sRGB of an Oklab colour, which may be outside 0 to 1.
fn light(lightness: f32, chroma: f32, hue: f32) -> [f32; 3] {
    let (a, b) = (chroma * hue.cos(), chroma * hue.sin());
    let l = (lightness + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m = (lightness - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s = (lightness - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// The colour of `lightness` and `hue` with as much of `chroma` as sRGB can
/// show.
fn shown(lightness: f32, chroma: f32, hue: f32) -> Rgba {
    let fits = |chroma| {
        light(lightness, chroma, hue)
            .iter()
            .all(|channel| (-1e-4..=1. + 1e-4).contains(channel))
    };
    let chroma = if fits(chroma) {
        chroma
    } else {
        // Halving finds the edge of sRGB to well under what the eye sees.
        let (mut inside, mut outside) = (0., chroma);
        for _ in 0..20 {
            let middle = (inside + outside) / 2.;
            if fits(middle) {
                inside = middle;
            } else {
                outside = middle;
            }
        }
        inside
    };
    let [r, g, b] = light(lightness, chroma, hue).map(|c| encoded(c.clamp(0., 1.)));
    Rgba { r, g, b, a: 1. }
}

/// A colour as `0xrrggbbaa`, as the palette writes its tokens.
pub(crate) fn hex(color: Rgba) -> u32 {
    let byte = |channel: f32| (channel.clamp(0., 1.) * 255.).round() as u32;
    byte(color.r) << 24 | byte(color.g) << 16 | byte(color.b) << 8 | byte(color.a)
}

/// An accent written `#rrggbb` or `rrggbb`, or `none` for the app's own.
pub(crate) fn parse(text: &str) -> Option<Option<Rgba>> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("none") {
        return Some(None);
    }
    let digits = text.strip_prefix('#').unwrap_or(text);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    Some(Some(rgba(value << 8 | 0xff)))
}

/// An accent as the XDG desktop portal gives it: sRGB channels, 0 to 1,
/// any other value saying the system has none.
#[allow(dead_code, reason = "Linux reads its accent from the portal")]
pub(crate) fn from_portal((r, g, b): (f64, f64, f64)) -> Option<Rgba> {
    let channel = |value: f64| (0.0..=1.0).contains(&value).then_some(value as f32);
    Some(Rgba {
        r: channel(r)?,
        g: channel(g)?,
        b: channel(b)?,
        a: 1.,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The palette's own looks: what is checked is what the app wears.
    use crate::theme::{DARK_LOOK as DARK, LIGHT_LOOK as LIGHT};

    /// Accents the systems offer, bright and pale ones among them.
    const ACCENTS: &[u32] = &[
        0x007affff, // a Mac's blue
        0xa550a7ff, // a Mac's purple
        0xf74f9eff, // a Mac's pink
        0xff5257ff, // a Mac's red
        0xf7821bff, // a Mac's orange
        0xffc600ff, // a Mac's yellow
        0x62ba46ff, // a Mac's green
        0x8c8c8cff, // a Mac's Graphite
        0x28c840ff, // a bright green
        0xff2d55ff, // a bright pink
        0x0078d4ff, // Windows' default
        0xffff00ff, // pure yellow
        0x00ffffff, // pure cyan
        0xffffffff, // white
        0x000000ff, // black
        0x3584e4ff, // GNOME's blue
        0xf6d32dff, // GNOME's yellow
    ];

    #[test]
    fn contrast_is_wcag_s() {
        assert!((contrast(rgba(0x000000ff), rgba(0xffffffff)) - 21.).abs() < 0.01);
        assert!((contrast(rgba(0x777777ff), rgba(0xffffffff)) - 4.48).abs() < 0.01);
        assert_eq!(contrast(rgba(0x336699ff), rgba(0x336699ff)), 1.);
    }

    #[test]
    fn oklab_goes_there_and_back() {
        for accent in ACCENTS {
            let color = rgba(*accent);
            let [l, c, h] = oklch(color);
            assert_eq!(hex(shown(l, c, h)), *accent, "{accent:08x}");
        }
    }

    #[test]
    fn every_accent_reads_on_every_ground_and_its_own_tint_as_drawn() {
        for look in [DARK, LIGHT] {
            for accent in ACCENTS {
                let derived = derive(rgba(*accent), &look);
                // As drawn: what is checked is what the renderer is given.
                assert_eq!(rgba(hex(derived)), derived);
                for ground in look.grounds {
                    let ground = rgba(*ground);
                    for under in [ground, over(ground, derived, look.tint)] {
                        let ratio = contrast(derived, under);
                        assert!(
                            ratio >= CONTRAST,
                            "{accent:08x} as {:08x} on {:08x}: {ratio:.2}",
                            hex(derived),
                            hex(under)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_control_s_accent_shows_a_white_knob() {
        for accent in ACCENTS {
            let control = control(rgba(*accent));
            let ratio = contrast(control, rgba(0xffffffff));
            assert!(ratio >= CONTROL_CONTRAST, "{accent:08x}: {ratio:.2}");
        }
        // A Mac's blue already does, and is left as it is.
        assert_eq!(hex(control(rgba(0x007affff))), 0x007affff);
        // A yellow is darkened, keeping its hue.
        let yellow = control(rgba(0xffc600ff));
        let [_, chroma, _] = oklch(yellow);
        assert!(hex(yellow) != 0xffc600ff && chroma > 0.05);
    }

    /// The systems' vivid accents, one of each hue.
    const VIVID: &[u32] = &[
        0x007affff, // blue
        0xaf52deff, // purple
        0xff2d55ff, // pink
        0xff5257ff, // red
        0xff9500ff, // orange
        0xffc600ff, // yellow
        0x28c840ff, // green
    ];

    /// What rounding a colour to bytes can add to its chroma or its
    /// lightness, or take away.
    const ROUNDING: f32 = 3e-3;

    /// How far apart two hues are, in radians.
    fn turned(a: f32, b: f32) -> f32 {
        let turned = (a - b).rem_euclid(std::f32::consts::TAU);
        turned.min(std::f32::consts::TAU - turned)
    }

    #[test]
    fn an_accent_keeps_its_hue_and_is_calmed() {
        for look in [DARK, LIGHT] {
            for accent in VIVID {
                let [_, system_chroma, system_hue] = oklch(rgba(*accent));
                let [_, chroma, hue] = oklch(derive(rgba(*accent), &look));
                // Never more than the look's, but for the rounding to bytes.
                assert!(chroma <= look.chroma.min(system_chroma) + ROUNDING);
                let turned = turned(hue, system_hue);
                assert!(turned < 0.05, "{accent:08x} turned by {turned}");
            }
        }
    }

    #[test]
    fn a_vivid_accent_stays_vivid_in_the_dark() {
        for accent in VIVID {
            let [_, chroma, _] = oklch(derive(rgba(*accent), &DARK));
            // All of what the look allows, which is well clear of a pastel.
            assert!(
                (chroma - DARK.chroma).abs() < ROUNDING && chroma > 0.1,
                "{accent:08x} kept {chroma}"
            );
        }
    }

    #[test]
    fn no_hue_is_louder_than_another_in_the_dark() {
        // Round the wheel, each hue as vivid as sRGB shows it: every one
        // keeps the look's chroma at the look's lightness, so none had to
        // be lightened, and paled, to be read.
        for degrees in (0..360).step_by(5) {
            let hue = (degrees as f32).to_radians();
            let [lightness, chroma, _] = oklch(derive(shown(0.7, 0.4, hue), &DARK));
            assert!(
                (chroma - DARK.chroma).abs() < ROUNDING,
                "{degrees}°: {chroma}"
            );
            assert!(
                (lightness - DARK.lightness).abs() < ROUNDING,
                "{degrees}°: {lightness}"
            );
        }
    }

    #[test]
    fn a_pale_accent_is_given_no_chroma_it_did_not_have() {
        // A slate, as GNOME's: quieter than the look allows, and left so.
        let [_, system_chroma, _] = oklch(rgba(0x6f8396ff));
        for look in [DARK, LIGHT] {
            let [_, chroma, _] = oklch(derive(rgba(0x6f8396ff), &look));
            assert!(system_chroma < look.chroma);
            assert!((chroma - system_chroma).abs() < ROUNDING);
        }
    }

    #[test]
    fn a_grey_accent_stays_grey() {
        for look in [DARK, LIGHT] {
            let [_, chroma, _] = oklch(derive(rgba(0x8c8c8cff), &look));
            assert!(chroma < 1e-3);
        }
    }

    #[test]
    fn a_grey_accent_has_no_hue_to_be_told_by() {
        for look in [DARK, LIGHT] {
            // A Mac's Graphite and GNOME's slate, and every vivid accent.
            assert!(!has_hue(derive(rgba(0x8c8c8cff), &look)));
            assert!(!has_hue(derive(rgba(0x6f8396ff), &look)));
            for accent in VIVID {
                assert!(has_hue(derive(rgba(*accent), &look)), "{accent:08x}");
            }
        }
    }

    #[test]
    fn a_yellow_accent_darkens_on_light_grounds() {
        let derived = derive(rgba(0xffc600ff), &LIGHT);
        let [lightness, _, _] = oklch(derived);
        assert!(lightness < LIGHT.lightness);
        // Still a yellow's hue, as an ochre, not a grey.
        let [_, chroma, _] = oklch(derived);
        assert!(chroma > 0.05);
    }

    #[test]
    fn a_blue_needs_no_moving_in_the_dark() {
        let [lightness, _, _] = oklch(derive(rgba(0x007affff), &DARK));
        assert!((lightness - DARK.lightness).abs() < 1e-3);
    }

    #[test]
    fn a_held_accent_is_hex_or_none() {
        assert_eq!(parse("#ffc600"), Some(Some(rgba(0xffc600ff))));
        assert_eq!(parse(" 3584E4 "), Some(Some(rgba(0x3584e4ff))));
        assert_eq!(parse("None"), Some(None));
        assert_eq!(parse("#fc0"), None);
        assert_eq!(parse("+12345"), None);
        assert_eq!(parse("blue"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn the_portal_s_accent_is_within_zero_and_one_or_none() {
        let accent = from_portal((0.2, 0.5, 1.0)).map(hex);
        assert_eq!(accent, Some(0x3380ffff));
        assert_eq!(from_portal((-1., -1., -1.)), None);
        assert_eq!(from_portal((0.2, 1.5, 0.4)), None);
        assert_eq!(from_portal((f64::NAN, 0., 0.)), None);
    }
}
