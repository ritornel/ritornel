//! One line of text with letter-spacing, which GPUI's text has not got
//! (an upstream pull request adds it: zed-industries/zed#53475, open). The
//! real design tightens every large heading (−0.034em on an album's title,
//! −0.022em on a page's name), and without it they set visibly looser.
//!
//! GPUI's shaped glyphs are public, so the line is shaped as GPUI would,
//! and each glyph is painted where it would be, moved by the tracking times
//! its place in the line. One line only, with no ellipsis: a heading that
//! does not fit is the caller's to cut. A line may be in more than one
//! colour, as Lean back's facts are (`tracked_runs`).

use gpui::{
    App, Bounds, Element, ElementId, FontWeight, GlobalElementId, Hsla, InspectorElementId,
    LayoutId, Pixels, ShapedLine, SharedString, Style, TextRun, Window, point, px, size,
};

pub(crate) struct Tracked {
    text: SharedString,
    size: f32,
    line_height: f32,
    weight: FontWeight,
    /// The colours, each for so many bytes of the text, in order.
    runs: Vec<(usize, Hsla)>,
    /// Points between letters, added: negative tightens.
    tracking: f32,
}

/// `text` at `size` points, `tracking_em` of the size between letters.
pub(crate) fn tracked(
    text: impl Into<SharedString>,
    size: f32,
    line_height: f32,
    weight: FontWeight,
    color: impl Into<Hsla>,
    tracking_em: f32,
) -> Tracked {
    let text = text.into();
    let runs = vec![(text.len(), color.into())];
    Tracked {
        text,
        size,
        line_height,
        weight,
        runs,
        tracking: tracking_em * size,
    }
}

/// The same, in more than one colour: `runs` are each a colour for so many
/// bytes of `text`, in order, and together cover all of it.
pub(crate) fn tracked_runs(
    text: impl Into<SharedString>,
    runs: Vec<(usize, Hsla)>,
    size: f32,
    line_height: f32,
    weight: FontWeight,
    tracking_em: f32,
) -> Tracked {
    Tracked {
        text: text.into(),
        size,
        line_height,
        weight,
        runs,
        tracking: tracking_em * size,
    }
}

impl Tracked {
    fn shape(&self, window: &Window) -> ShapedLine {
        let mut font = window.text_style().font();
        font.weight = self.weight;
        let runs: Vec<TextRun> = self
            .runs
            .iter()
            .map(|(len, color)| TextRun {
                len: *len,
                font: font.clone(),
                color: *color,
                background_color: None,
                underline: None,
                strikethrough: None,
            })
            .collect();
        window
            .text_system()
            .shape_line(self.text.clone(), px(self.size), &runs, None)
    }

    /// The colour of the glyph from the byte at `index`.
    fn color_at(&self, index: usize) -> Hsla {
        let mut end = 0;
        for (len, color) in &self.runs {
            end += len;
            if index < end {
                return *color;
            }
        }
        self.runs
            .last()
            .map_or_else(Hsla::default, |(_, color)| *color)
    }

    fn glyphs(line: &ShapedLine) -> usize {
        line.runs.iter().map(|run| run.glyphs.len()).sum()
    }

    /// The width the line takes, tracked.
    pub(crate) fn width(&self, window: &Window) -> f32 {
        let line = self.shape(window);
        f32::from(line.width) + self.tracking * Self::glyphs(&line).saturating_sub(1) as f32
    }
}

impl Element for Tracked {
    type RequestLayoutState = ShapedLine;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ShapedLine) {
        let line = self.shape(window);
        let width =
            f32::from(line.width) + self.tracking * Self::glyphs(&line).saturating_sub(1) as f32;
        let style = Style {
            size: size(px(width.max(0.)).into(), px(self.line_height).into()),
            flex_shrink: 0.,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), line)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _line: &mut ShapedLine,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        line: &mut ShapedLine,
        _prepaint: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) {
        // The baseline, as GPUI centres a line in its height.
        let (ascent, descent) = (f32::from(line.ascent), f32::from(line.descent));
        let baseline =
            f32::from(bounds.origin.y) + (self.line_height - (ascent + descent)) / 2. + ascent;
        let mut at = 0usize;
        for run in &line.runs {
            for glyph in &run.glyphs {
                let x = f32::from(bounds.origin.x)
                    + f32::from(glyph.position.x)
                    + self.tracking * at as f32;
                let _ = window.paint_glyph(
                    point(px(x), px(baseline)),
                    run.font_id,
                    glyph.id,
                    px(self.size),
                    self.color_at(glyph.index),
                );
                at += 1;
            }
        }
    }
}

/// Paints `text` as `tracked` would, its middle at `centre` and its
/// baseline at `baseline`: a name written on a picture, as SVG's
/// `text-anchor="middle"` sets it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_centred(
    text: impl Into<SharedString>,
    size: f32,
    weight: FontWeight,
    color: impl Into<Hsla>,
    tracking_em: f32,
    centre: f32,
    baseline: f32,
    window: &mut Window,
) {
    // One colour: the line's runs are of it alone.
    let color: Hsla = color.into();
    let line = tracked(text, size, size, weight, color, tracking_em);
    let shaped = line.shape(window);
    let width =
        f32::from(shaped.width) + line.tracking * Tracked::glyphs(&shaped).saturating_sub(1) as f32;
    let left = centre - width / 2.;
    let mut at = 0usize;
    for run in &shaped.runs {
        for glyph in &run.glyphs {
            let x = left + f32::from(glyph.position.x) + line.tracking * at as f32;
            let _ = window.paint_glyph(
                point(px(x), px(baseline)),
                run.font_id,
                glyph.id,
                px(line.size),
                color,
            );
            at += 1;
        }
    }
}
