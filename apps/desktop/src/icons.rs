//! The real app's icons (`apps/desktop-tauri/src/lib/icons.ts`): one path each on
//! a 16-point square, filled, or drawn as a 1.5-point line. GPUI draws an
//! SVG as a mask in the element's text colour, as the real app's
//! `currentColor` does. Served as `icons/<name>.svg`.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

const ICONS: &[(&str, &str, bool)] = &[
    (
        "back",
        "M10.2 2.6 4.8 8l5.4 5.4 1.1-1.1L7 8l4.3-4.3z",
        false,
    ),
    (
        "search",
        "M7 2a5 5 0 0 1 4 8l3 3-1.1 1.1-3-3A5 5 0 1 1 7 2zm0 1.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z",
        false,
    ),
    ("play", "M4 2.2v11.6L13.5 8z", false),
    ("pause", "M3.5 2.5h3.2v11H3.5zM9.3 2.5h3.2v11H9.3z", false),
    ("previous", "M2.5 3h1.8v10H2.5zM13.5 3v10L5.5 8z", false),
    ("next", "M11.7 3h1.8v10h-1.8zM2.5 3v10l8-5z", false),
    (
        "shuffle",
        "M1.5 4.5h2.4c1.2 0 2.3.6 2.9 1.6l2.4 3.8c.6 1 1.7 1.6 2.9 1.6h2.4M12.7 9.3l2 2.2-2 2.2M1.5 11.5h2.4c.9 0 1.8-.4 2.4-1M9.4 5.5c.6-.6 1.5-1 2.4-1h2.9M12.7 2.3l2 2.2-2 2.2",
        true,
    ),
    (
        "repeat",
        "M2.5 7.5V6.2a2.7 2.7 0 0 1 2.7-2.7h8.3M11.5 1.5l2 2-2 2M13.5 8.5v1.3a2.7 2.7 0 0 1-2.7 2.7H2.5M4.5 14.5l-2-2 2-2",
        true,
    ),
    (
        "upNext",
        "M2 3h9v1.6H2zm0 4h9v1.6H2zm0 4h6v1.6H2zm9.5-.9 3.5 2.2-3.5 2.2z",
        false,
    ),
    (
        "dots",
        "M1.5 8a1.5 1.5 0 1 0 3 0 1.5 1.5 0 1 0-3 0zm5 0a1.5 1.5 0 1 0 3 0 1.5 1.5 0 1 0-3 0zm5 0a1.5 1.5 0 1 0 3 0 1.5 1.5 0 1 0-3 0z",
        false,
    ),
    ("plus", "M7 2h2v5h5v2H9v5H7V9H2V7h5z", false),
    (
        "check",
        "M6.2 11.3 2.9 8l-1.4 1.4 4.7 4.7L14.5 5.8l-1.4-1.4z",
        false,
    ),
    (
        "close",
        "m3.4 2 4.6 4.6L12.6 2 14 3.4 9.4 8l4.6 4.6-1.4 1.4L8 9.4 3.4 14 2 12.6 6.6 8 2 3.4z",
        false,
    ),
    (
        "leanBack",
        "M2 2h5v1.6H4.7l3 3-1.1 1.1-3-3V7H2zm12 12H9v-1.6h2.3l-3-3 1.1-1.1 3 3V9H14z",
        false,
    ),
    // The sort menu's chevron, from the real Library page's CSS.
    ("chevron", "M4 6.2 8 10.2l4-4", true),
];

/// Whole pictures, not one path on the 16-point square: the Settings
/// window's (`settings/Settings.svelte`, `Library.svelte`), on squares of
/// their own, and the parts of its spinner (`settings.css`, `.spin`).
const PICTURES: &[(&str, &str)] = &[
    (
        "settings/library",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M9 18V5l11-2v13M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0zm11-2a3 3 0 1 1-6 0 3 3 0 0 1 6 0z" fill="none" stroke="black" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
    ),
    (
        "settings/appearance",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zm0-20v20m0-15h8.5M12 12h10m-10 5h8.5" fill="none" stroke="black" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
    ),
    (
        "settings/lastfm",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M16.24 7.76a6 6 0 0 1 0 8.49m-8.48 0a6 6 0 0 1 0-8.49m11.31-2.83a10 10 0 0 1 0 14.15m-14.14 0a10 10 0 0 1 0-14.15" fill="none" stroke="black" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/><circle cx="12" cy="12" r="2" fill="black"/></svg>"#,
    ),
    (
        "settings/about",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zm0-6v-4.5m0-3.5h.01" fill="none" stroke="black" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
    ),
    (
        "settings/folder",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 16"><path d="M1.5 3.5A1.5 1.5 0 0 1 3 2h4.2l1.6 1.8H17a1.5 1.5 0 0 1 1.5 1.5v7.7A1.5 1.5 0 0 1 17 14.5H3A1.5 1.5 0 0 1 1.5 13z" fill="black"/></svg>"#,
    ),
    // The spinner: its ring, and the quarter of it that turns, as CSS
    // draws a circle's border with a top border of another colour.
    (
        "settings/ring",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14"><path fill-rule="evenodd" d="M7 0a7 7 0 1 1 0 14A7 7 0 1 1 7 0zm0 2a5 5 0 1 0 0 10A5 5 0 1 0 7 2z" fill="black"/></svg>"#,
    ),
    (
        "settings/arc",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14"><path d="M2.05 2.05a7 7 0 0 1 9.9 0l-1.41 1.41a5 5 0 0 0-7.08 0z" fill="black"/></svg>"#,
    ),
];

pub(crate) struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let name = path
            .strip_prefix("icons/")
            .and_then(|rest| rest.strip_suffix(".svg"));
        if let Some((_, svg)) = PICTURES.iter().find(|(picture, _)| Some(*picture) == name) {
            return Ok(Some(Cow::Borrowed(svg.as_bytes())));
        }
        let Some((_, d, stroke)) = ICONS.iter().find(|(icon, _, _)| Some(*icon) == name) else {
            return Ok(None);
        };
        let paint = if *stroke {
            r#"fill="none" stroke="black" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round""#
        } else {
            r#"fill="black""#
        };
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="{d}" {paint}/></svg>"#
        );
        Ok(Some(Cow::Owned(svg.into_bytes())))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _, _)| *name)
            .chain(PICTURES.iter().map(|(name, _)| *name))
            .map(|name| SharedString::from(format!("icons/{name}.svg")))
            .collect())
    }
}

/// An icon `size` points square, in `color`. GPUI's `svg` takes its colour
/// from its own style only, not from its parent's as CSS's `currentColor`
/// does, so it is always given one.
pub(crate) fn icon(name: &str, size: f32, color: impl Into<gpui::Hsla>) -> gpui::Svg {
    use gpui::Styled;
    gpui::svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(gpui::px(size))
        .flex_none()
        .text_color(color)
}
