//! The small drawings of what the library syncs to: a player or a folder
//! drawn as the thing itself, in its own colours, about 26 points in a
//! device's tab and 62 on its page ("Devices" in docs/internals/desktop.md).
//!
//! Each is an SVG made here and drawn as a picture, since GPUI's own `svg`
//! element draws one colour. A drawing is known by a key, a short lowercase
//! word path such as `folder.car_stereo` or `player.ipod_video.white`, which
//! a server names; a key this build has no drawing for falls back along the
//! path, and at last to a plain folder or a plain player.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Image, ImageFormat, IntoElement, ObjectFit, Styled, StyledImage, img, px};

use crate::theme;

/// What a drawing is, and its width for a height of one.
struct Drawing {
    svg: String,
    width: f32,
    height: f32,
}

fn hex(color: gpui::Rgba) -> String {
    let byte = |part: f32| (part.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b)
    )
}

fn boxed(width: f32, height: f32, body: String) -> Drawing {
    Drawing {
        svg: body,
        width,
        height,
    }
}

/// A linear gradient from the top left to the bottom right.
fn across(id: &str, from: &str, to: &str) -> String {
    format!(
        r#"<linearGradient id="{id}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{from}"/><stop offset="1" stop-color="{to}"/></linearGradient>"#
    )
}

/// A linear gradient from the top to the bottom.
fn down(id: &str, from: &str, to: &str) -> String {
    format!(
        r#"<linearGradient id="{id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{from}"/><stop offset="1" stop-color="{to}"/></linearGradient>"#
    )
}

/// An iPod Video, in white or black.
fn ipod(black: bool, edge: &str) -> Drawing {
    let (body, body2, wheel, hub, marks) = if black {
        ("#3a3b3e", "#1c1d1f", "#2a2b2e", "#3a3b3e", "#8a8b90")
    } else {
        ("#fbfbfa", "#e4e4e1", "#efefec", "#fbfbfa", "#9a9a96")
    };
    boxed(
        64.,
        106.,
        format!(
            r##"<defs>{}{}</defs>
<rect x="1" y="1" width="62" height="104" rx="7.5" fill="url(#b)" stroke="{edge}" stroke-width="1"/>
<rect x="7" y="7.5" width="50" height="38" rx="1.6" fill="url(#s)"/><rect x="7" y="7.5" width="50" height="5" rx="1.2" fill="#c9d2dd" opacity=".9"/>
<rect x="10" y="16" width="26" height="2" rx="1" fill="#fff" opacity=".55"/><rect x="10" y="21" width="20" height="2" rx="1" fill="#fff" opacity=".35"/><rect x="10" y="26" width="23" height="2" rx="1" fill="#fff" opacity=".35"/><rect x="10" y="31" width="17" height="2" rx="1" fill="#fff" opacity=".35"/>
<circle cx="32" cy="76" r="18" fill="{wheel}" stroke="#000" stroke-opacity=".13" stroke-width="1"/><circle cx="32" cy="76" r="6.6" fill="{hub}" stroke="#000" stroke-opacity=".14" stroke-width="1"/>
<g fill="{marks}"><rect x="29.2" y="60.2" width="5.6" height="1.3" rx=".6"/><path d="M18 74.8v2.4l-2.2-1.2zM19.2 74.8v2.4l-2.2-1.2z"/><path d="M44.8 74.8v2.4l2.2-1.2zM46 74.8v2.4l2.2-1.2z"/><path d="M29.4 90.6v2.6l2.2-1.3zM32.6 90.6h.9v2.6h-.9zM34.2 90.6h.9v2.6h-.9z"/></g>"##,
            across("b", body, body2),
            down("s", "#2a3441", "#151a22"),
        ),
    )
}

/// A Zune 30, in brown.
fn zune(edge: &str) -> Drawing {
    boxed(
        64.,
        114.,
        format!(
            r##"<defs>{}{}</defs>
<rect x="1" y="1" width="62" height="112" rx="8" fill="url(#b)" stroke="{edge}" stroke-width="1"/>
<rect x="7" y="8" width="50" height="66" rx="2" fill="url(#s)"/>
<rect x="11" y="16" width="16" height="3" rx="1.5" fill="#fff" opacity=".8"/>
<rect x="11" y="29" width="22" height="2" rx="1" fill="#fff" opacity=".4"/><rect x="11" y="34" width="28" height="2" rx="1" fill="#fff" opacity=".3"/><rect x="11" y="39" width="18" height="2" rx="1" fill="#fff" opacity=".3"/><rect x="11" y="44" width="25" height="2" rx="1" fill="#fff" opacity=".3"/>
<circle cx="32" cy="92" r="11.5" fill="#3d281c" stroke="#fff" stroke-opacity=".14" stroke-width="1"/><circle cx="32" cy="92" r="6" fill="#55392a" stroke="#000" stroke-opacity=".25" stroke-width=".8"/>
<g fill="#c9b3a3" opacity=".8"><path d="M12.5 92l3-2.2v4.4z"/><path d="M48.2 89.8v4.4l2.8-2.2zM52 89.8h.9v4.4H52zM53.6 89.8h.9v4.4h-.9z"/></g>"##,
            across("b", "#6b4a35", "#4a3123"),
            down("s", "#1c1c1e", "#0e0e10"),
        ),
    )
}

/// A plain player: a dark slab with a screen and a wheel.
fn player(edge: &str) -> Drawing {
    boxed(
        64.,
        106.,
        format!(
            r##"<defs>{}{}</defs>
<rect x="1" y="1" width="62" height="104" rx="8" fill="url(#b)" stroke="{edge}" stroke-width="1"/>
<rect x="7" y="8" width="50" height="46" rx="2" fill="url(#s)"/>
<rect x="11" y="15" width="24" height="2.2" rx="1.1" fill="#fff" opacity=".55"/><rect x="11" y="21" width="18" height="2" rx="1" fill="#fff" opacity=".32"/><rect x="11" y="27" width="21" height="2" rx="1" fill="#fff" opacity=".32"/>
<rect x="11" y="44" width="42" height="2.4" rx="1.2" fill="#fff" opacity=".18"/><rect x="11" y="44" width="17" height="2.4" rx="1.2" fill="#8fb4d9"/>
<circle cx="32" cy="79" r="14" fill="#2c2f36" stroke="#fff" stroke-opacity=".12" stroke-width="1"/><circle cx="32" cy="79" r="5" fill="#3b3f48"/>"##,
            across("b", "#4a4e57", "#24262b"),
            down("s", "#232c38", "#11161d"),
        ),
    )
}

/// A folder, as the system draws one, in a quiet blue.
fn folder(edge: &str) -> Drawing {
    boxed(
        100.,
        80.,
        format!(
            r##"<defs>{}{}</defs>
<path d="M6 12a6 6 0 0 1 6-6h24l9 9h43a6 6 0 0 1 6 6v8H6z" fill="url(#t)" stroke="{edge}" stroke-width="1"/>
<rect x="3" y="22" width="94" height="54" rx="6" fill="url(#f)" stroke="{edge}" stroke-width="1"/>
<rect x="3.5" y="22.5" width="93" height="3" rx="1.5" fill="#fff" opacity=".28"/>
<g fill="#fff" opacity=".9"><circle cx="43" cy="57" r="5"/><circle cx="59" cy="54" r="5"/><path d="M46 38.5l16-3.5v4l-12.4 2.7V57h-3.6z"/><path d="M58.4 37.5H62V54h-3.6z"/></g>"##,
            down("t", "#5d9fe0", "#4a8dd2"),
            down("f", "#79b6f2", "#4f93d9"),
        ),
    )
}

/// A drive: a USB stick lying on its side, capped in metal.
fn drive(edge: &str) -> Drawing {
    boxed(
        110.,
        56.,
        format!(
            r##"<defs>{}{}</defs>
<rect x="74" y="13" width="33" height="30" rx="2.5" fill="url(#m)" stroke="{edge}" stroke-width="1"/>
<rect x="85" y="20" width="8" height="6" rx="1" fill="#70757d"/><rect x="85" y="30" width="8" height="6" rx="1" fill="#70757d"/>
<rect x="3" y="5" width="74" height="46" rx="9" fill="url(#b)" stroke="{edge}" stroke-width="1"/>
<rect x="3.5" y="5.5" width="73" height="4" rx="2" fill="#fff" opacity=".1"/>
<circle cx="18" cy="28" r="5.5" fill="none" stroke="#fff" stroke-opacity=".22" stroke-width="2.4"/>
<rect x="34" y="24" width="26" height="2.6" rx="1.3" fill="#fff" opacity=".4"/><rect x="34" y="30" width="18" height="2.6" rx="1.3" fill="#fff" opacity=".25"/>"##,
            across("b", "#3e424b", "#1f2126"),
            down("m", "#e6e8eb", "#b3b7be"),
        ),
    )
}

/// A car stereo: a head unit, one slot wide, with a display and a knob.
fn car_stereo(edge: &str) -> Drawing {
    boxed(
        120.,
        54.,
        format!(
            r##"<defs>{}{}</defs>
<rect x="2" y="3" width="116" height="48" rx="6" fill="url(#b)" stroke="{edge}" stroke-width="1"/>
<circle cx="22" cy="27" r="12.5" fill="#17181b" stroke="#fff" stroke-opacity=".16" stroke-width="1"/><circle cx="22" cy="27" r="7" fill="#2e3138"/><rect x="21.2" y="16.5" width="1.6" height="5" rx=".8" fill="#e0ad48"/>
<rect x="42" y="11" width="56" height="20" rx="2.5" fill="url(#s)"/>
<rect x="46" y="16" width="20" height="2.4" rx="1.2" fill="#9fd0ff" opacity=".9"/><rect x="46" y="22" width="32" height="2" rx="1" fill="#9fd0ff" opacity=".45"/>
<g fill="#4b4f58"><rect x="42" y="37" width="10" height="7" rx="1.6"/><rect x="55.5" y="37" width="10" height="7" rx="1.6"/><rect x="69" y="37" width="10" height="7" rx="1.6"/><rect x="82.5" y="37" width="10" height="7" rx="1.6"/></g>
<rect x="103" y="14" width="9" height="26" rx="2" fill="#17181b"/><rect x="105.5" y="19" width="4" height="9" rx="1" fill="#70757d"/>"##,
            across("b", "#33363d", "#1b1d21"),
            down("s", "#0f2233", "#0a1622"),
        ),
    )
}

/// A phone, face on.
fn phone(edge: &str) -> Drawing {
    boxed(
        56.,
        110.,
        format!(
            r##"<defs>{}{}</defs>
<rect x="1" y="1" width="54" height="108" rx="10" fill="url(#b)" stroke="{edge}" stroke-width="1"/>
<rect x="4.5" y="4.5" width="47" height="101" rx="7" fill="url(#s)"/>
<rect x="21" y="8" width="14" height="4" rx="2" fill="#000" opacity=".55"/>
<rect x="10" y="24" width="36" height="36" rx="5" fill="#8fb4d9" opacity=".85"/>
<rect x="10" y="68" width="24" height="2.6" rx="1.3" fill="#fff" opacity=".7"/><rect x="10" y="74" width="17" height="2.2" rx="1.1" fill="#fff" opacity=".38"/>
<rect x="10" y="84" width="36" height="2.2" rx="1.1" fill="#fff" opacity=".18"/><rect x="10" y="84" width="15" height="2.2" rx="1.1" fill="#fff" opacity=".75"/>
<rect x="20" y="100" width="16" height="2" rx="1" fill="#fff" opacity=".45"/>"##,
            across("b", "#3c3f46", "#1d1f23"),
            down("s", "#1b2430", "#0e1319"),
        ),
    )
}

/// The drawing for a key, falling back along its path.
fn drawing(key: &str, edge: &str) -> Drawing {
    match key {
        "player.ipod_video.black" => ipod(true, edge),
        "player.ipod_video.white" | "player.ipod_video" => ipod(false, edge),
        "player.zune_30.brown" | "player.zune_30" | "player.zune" => zune(edge),
        "folder.car_stereo" => car_stereo(edge),
        "folder.phone" => phone(edge),
        "folder.player" => player(edge),
        "folder.drive" => drive(edge),
        "folder" => folder(edge),
        "player" | "rockbox" | "zune" => player(edge),
        other => match other.rsplit_once('.') {
            Some((above, _)) => drawing(above, edge),
            None => folder(edge),
        },
    }
}

/// A drawing as it is shown: the picture, and its width for a height of one.
type Made = (Arc<Image>, f32);

thread_local! {
    /// The drawings made so far, by key and appearance.
    static MADE: RefCell<HashMap<(String, bool), Made>> = RefCell::new(HashMap::new());
}

/// How many pixels a drawing is made at for each point of its height, so
/// that it is sharp at the page's size on any screen.
const SHARP: f32 = 4.;
/// The tallest a drawing is shown, in points.
const TALLEST: f32 = 70.;

/// The drawing for `key`, as large as fits in `width` by `height` points:
/// a tall player takes the height, and a wide stereo or stick the width.
pub(crate) fn art(key: &str, width: f32, height: f32) -> impl IntoElement {
    let dark = theme::dark();
    let (image, aspect) = MADE.with(|made| {
        made.borrow_mut()
            .entry((key.to_owned(), dark))
            .or_insert_with(|| {
                // The edge that sets a pale drawing apart from a pale page.
                let edge = hex(theme::mix(theme::bg(), theme::ink(), 0.22));
                let drawn = drawing(key, &edge);
                let scale = TALLEST * SHARP / drawn.height;
                let svg = format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}" width="{}" height="{}">{}</svg>"#,
                    drawn.width,
                    drawn.height,
                    (drawn.width * scale).round(),
                    (drawn.height * scale).round(),
                    drawn.svg
                );
                (
                    Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
                    drawn.width / drawn.height,
                )
            })
            .clone()
    });
    let height = height.min(width / aspect);
    img(image)
        .h(px(height.round()))
        .w(px((height * aspect).round()))
        .flex_none()
        .object_fit(ObjectFit::Contain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_with_no_drawing_falls_back_along_its_path() {
        let same = |a: &str, b: &str| drawing(a, "#000").svg == drawing(b, "#000").svg;
        assert!(same("player.ipod_video.white", "player.ipod_video"));
        assert!(same("folder.car_stereo.pioneer", "folder.car_stereo"));
        assert!(same("player.sansa_clip.blue", "player"));
        assert!(same("something.new", "folder"));
        assert!(!same("folder.phone", "folder"));
    }

    #[test]
    fn every_drawing_is_an_svg_usvg_can_read_at_its_own_size() {
        for key in [
            "folder",
            "folder.drive",
            "folder.car_stereo",
            "folder.phone",
            "folder.player",
            "player.ipod_video.white",
            "player.ipod_video.black",
            "player.zune_30.brown",
        ] {
            let drawn = drawing(key, "#445566");
            assert!(drawn.width > 0. && drawn.height > 0., "{key}");
            // Every id a fill names is defined in the drawing.
            for id in ["b", "s", "t", "f", "m"] {
                if drawn.svg.contains(&format!("url(#{id})")) {
                    assert!(drawn.svg.contains(&format!(r#"id="{id}""#)), "{key}: {id}");
                }
            }
        }
    }
}
