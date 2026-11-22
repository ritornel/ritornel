//! History as one picture of your listening life ("History tab and Lean
//! back" in docs/internals/desktop.md): time runs left to right, the stream
//! is as tall as you listened, and each layer is the artists you first
//! heard in one year, the oldest at the bottom and the newest on top, in
//! one hue shaded from one to the other. The line is smoothed over about
//! half a year, and its height is the square root of the plays, so that the
//! quiet years still read: it is a picture, not a measure. Its tests come
//! with it.

use ritornel_contract::{HistoryStream, Phrase};

use crate::words::plural;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Where a layer's name sits, and how large.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Label {
    pub x: f32,
    /// Its baseline.
    pub y: f32,
    pub size: f32,
}

/// One layer as it is drawn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Layer {
    /// The year its artists were first heard.
    pub year: i32,
    /// What it is called: the year, or for the first, that year and before.
    pub name: String,
    /// Its upper and lower edges, a point a month, in the picture's points.
    pub top: Vec<f32>,
    pub bottom: Vec<f32>,
    /// How much of the hue it takes, in percent: more for newer years.
    pub shade: f32,
    /// Its name, as large as sits inside it, where it has most room. `None`
    /// when nowhere does.
    pub label: Option<Label>,
}

/// One year's name along the bottom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct YearMark {
    pub year: i32,
    pub x: f32,
}

/// The picture, drawn for a width and a height.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Drawn {
    pub width: f32,
    pub height: f32,
    /// How far apart its months are.
    pub step: f32,
    /// The newest first: the order they are stacked in, from the top.
    pub layers: Vec<Layer>,
    /// Where each year's name stands along the bottom: under its middle.
    pub years: Vec<YearMark>,
}

/// The first month, as a number that counts months: year × 12 + month from 0.
pub(crate) fn base(stream: &HistoryStream) -> i32 {
    let from = stream.from.as_deref().unwrap_or("1970-01");
    let mut parts = from.split('-').map(|part| part.parse::<i32>().ok());
    let year = parts.next().flatten().unwrap_or(1970);
    let month = parts.next().flatten().unwrap_or(1);
    year * 12 + month - 1
}

/// "March 2016", for the month `at` from the first.
pub(crate) fn month_name(stream: &HistoryStream, at: usize) -> String {
    let month = base(stream) + at as i32;
    format!(
        "{} {}",
        MONTHS.get(month.rem_euclid(12) as usize).unwrap_or(&""),
        month.div_euclid(12)
    )
}

/// How many years the history runs over.
pub(crate) fn years_of(stream: &HistoryStream) -> u64 {
    if stream.months == 0 {
        return 0;
    }
    let first = base(stream);
    let last = first + stream.months as i32 - 1;
    (last.div_euclid(12) - first.div_euclid(12) + 1) as u64
}

/// The year of the last month: this one, by the server's clock.
pub(crate) fn this_year(stream: &HistoryStream) -> Option<i32> {
    (stream.months > 0).then(|| (base(stream) + stream.months as i32 - 1).div_euclid(12))
}

/// Smoothed over about half a year (a bell of 3.6 months either way), so
/// the line flows and one busy week does not spike.
pub(crate) fn smooth(values: &[f32]) -> Vec<f32> {
    const REACH: i64 = 9;
    let kernel: Vec<f32> = (0..=2 * REACH)
        .map(|k| (-((k - REACH) as f32).powi(2) / (2. * 3.6 * 3.6)).exp())
        .collect();
    (0..values.len() as i64)
        .map(|month| {
            let (mut sum, mut weight) = (0f32, 0f32);
            for k in -REACH..=REACH {
                let Some(value) = usize::try_from(month + k)
                    .ok()
                    .and_then(|at| values.get(at))
                else {
                    continue;
                };
                let w = kernel.get((k + REACH) as usize).copied().unwrap_or(0.);
                sum += value * w;
                weight += w;
            }
            if weight == 0. { 0. } else { sum / weight }
        })
        .collect()
}

/// How far from a label's middle a year under the stream reaches, near enough.
const YEAR_HALF: f32 = 18.;
/// The least room from one year's middle to the next under the stream.
const YEAR_GAP: f32 = 2. * YEAR_HALF + 14.;

/// Where something `half` wide either way can stand across `width`,
/// keeping `inset` clear at each end.
pub(crate) fn within(x: f32, half: f32, width: f32, inset: f32) -> f32 {
    x.max(inset + half).min(width - inset - half)
}

fn smoothed(plays: &[u64]) -> Vec<f32> {
    smooth(&plays.iter().map(|&value| value as f32).collect::<Vec<_>>())
}

/// Draws the stream `width` points across and `height` high, its months
/// across the whole width. What is written on it and under it keeps
/// `inset` clear at each end: the page's own margins, where its text is.
pub(crate) fn draw(stream: &HistoryStream, width: f32, height: f32, inset: f32) -> Drawn {
    let months = stream.months as usize;
    if months < 2 || width <= 0. || height <= 0. {
        return Drawn {
            width,
            height,
            step: 0.,
            layers: Vec::new(),
            years: Vec::new(),
        };
    }
    let step = width / (months - 1) as f32;
    let tall: Vec<f32> = smoothed(&stream.total).into_iter().map(f32::sqrt).collect();
    let scale = height * 0.97 / tall.iter().copied().fold(1e-9, f32::max);
    let mut newest: Vec<_> = stream.layers.iter().collect();
    newest.sort_by_key(|layer| std::cmp::Reverse(layer.year));
    let values: Vec<Vec<f32>> = newest.iter().map(|layer| smoothed(&layer.plays)).collect();
    let named: Vec<f32> = (0..months)
        .map(|month| {
            values
                .iter()
                .map(|v| v.get(month).copied().unwrap_or(0.))
                .sum()
        })
        .collect();
    let first = newest.last().map_or(0, |layer| layer.year);
    let span = (newest.first().map_or(first, |layer| layer.year) - first).max(1) as f32;
    // From the top edge of the stream, centred on the middle, down.
    let mut edge: Vec<f32> = (0..months)
        .map(|month| height / 2. - tall.get(month).copied().unwrap_or(0.) * scale / 2.)
        .collect();
    let layers = newest
        .iter()
        .zip(&values)
        .map(|(layer, v)| {
            let top = edge.clone();
            for (month, (at, &all)) in edge.iter_mut().zip(&named).enumerate() {
                if all > 0. {
                    *at += v.get(month).copied().unwrap_or(0.) / all
                        * tall.get(month).copied().unwrap_or(0.)
                        * scale;
                }
            }
            let bottom = edge.clone();
            let name = if layer.year == first {
                format!("{} and before", layer.year)
            } else {
                layer.year.to_string()
            };
            let label = label(&name, &top, &bottom, step, inset);
            Layer {
                year: layer.year,
                name,
                top,
                bottom,
                shade: (12. + 58. * (layer.year - first) as f32 / span).round(),
                label,
            }
        })
        .collect();
    Drawn {
        width,
        height,
        step,
        layers,
        years: axis_of(stream, step, width, inset),
    }
}

/// The years under the stream, each under its middle, kept inside the
/// margins. A year at an end that the margin moves in nudges its
/// neighbours along rather than crowding them, so the first and the last
/// are always there; with too little room for every year, every other.
fn axis_of(stream: &HistoryStream, step: f32, width: f32, inset: f32) -> Vec<YearMark> {
    let start = base(stream);
    let months = stream.months as i32;
    let mut all = Vec::new();
    let mut year = start.div_euclid(12) + 1;
    while year * 12 - start < months {
        all.push(YearMark {
            year,
            x: (year * 12 - start + 6) as f32 * step,
        });
        year += 1;
    }
    let room = width - 2. * inset - 2. * YEAR_HALF;
    let every =
        ((YEAR_GAP * all.len().saturating_sub(1) as f32 / room.max(1.)).ceil() as usize).max(1);
    let last = all.len().saturating_sub(1);
    let shown: Vec<YearMark> = all
        .into_iter()
        .enumerate()
        .filter(|(at, _)| at % every == 0 || *at == last)
        .map(|(_, mark)| mark)
        .collect();
    // From the left, then from the right, each at least a gap from the last.
    let inside = |at: &mut Vec<f32>| {
        for x in at.iter_mut() {
            *x = within(*x, YEAR_HALF, width, inset);
        }
    };
    let mut at: Vec<f32> = shown
        .iter()
        .map(|mark| within(mark.x, YEAR_HALF, width, inset))
        .collect();
    let mut before = f32::NEG_INFINITY;
    for x in at.iter_mut() {
        *x = x.max(before + YEAR_GAP);
        before = *x;
    }
    inside(&mut at);
    let mut after = f32::INFINITY;
    for x in at.iter_mut().rev() {
        *x = x.min(after - YEAR_GAP);
        after = *x;
    }
    inside(&mut at);
    let mut marks: Vec<YearMark> = shown
        .iter()
        .zip(at)
        .map(|(mark, x)| YearMark { year: mark.year, x })
        .collect();
    // The last year may come too close to the one before it when only
    // every other is shown: then that one gives way.
    if let [.., before, end] = marks.as_slice()
        && end.x - before.x < YEAR_GAP - 1.
    {
        marks.remove(marks.len() - 2);
    }
    marks
}

/// How far a layer runs on past each end, level: the picture is cut at its
/// edges, so a layer meets them in full, and its sides, which would be
/// stroked, are out of sight.
pub(crate) const BLEED: f32 = 2.;

/// A layer's outline: along its top from left to right, a little past each
/// end, and back along its bottom.
pub(crate) fn outline(top: &[f32], bottom: &[f32], step: f32) -> Vec<(f32, f32)> {
    let (Some(&first_top), Some(&last_top), Some(&first_bottom), Some(&last_bottom)) =
        (top.first(), top.last(), bottom.first(), bottom.last())
    else {
        return Vec::new();
    };
    let last = (top.len() - 1) as f32 * step;
    let mut points = Vec::with_capacity(2 * top.len() + 4);
    points.push((-BLEED, first_top));
    points.extend(top.iter().enumerate().map(|(m, &y)| (m as f32 * step, y)));
    points.push((last + BLEED, last_top));
    points.push((last + BLEED, last_bottom));
    points.extend(
        bottom
            .iter()
            .enumerate()
            .rev()
            .map(|(m, &y)| (m as f32 * step, y)),
    );
    points.push((-BLEED, first_bottom));
    points
}

/// Where a layer's name sits: as large as fits inside it, from 28 points
/// down to 9, where it has most room, and never in the margins.
fn label(name: &str, top: &[f32], bottom: &[f32], step: f32, inset: f32) -> Option<Label> {
    let months = top.len() as i64;
    // A little more than the margin, as a name is measured only near enough.
    let clear = ((inset + 8.) / step).ceil() as i64;
    let length = name.encode_utf16().count() as f32;
    let at_ = |edges: &[f32], at: i64| {
        usize::try_from(at)
            .ok()
            .and_then(|at| edges.get(at))
            .copied()
            .unwrap_or(0.)
    };
    let mut size = 28.;
    while size >= 9. {
        let half = (length * size * 0.285 / step).ceil() as i64;
        let (mut room, mut at, mut middle) = (0f32, -1i64, 0f32);
        for month in clear + half..months - clear - half {
            if at_(bottom, month) - at_(top, month) < size * 1.15 {
                continue;
            }
            let (mut low, mut high) = (-1e9f32, 1e9f32);
            for near in month - half..=month + half {
                low = low.max(at_(top, near));
                high = high.min(at_(bottom, near));
            }
            if high - low > room {
                room = high - low;
                at = month;
                middle = (low + high) / 2.;
            }
        }
        if room >= size * 1.12 {
            return Some(Label {
                x: at as f32 * step,
                y: middle + size * 0.35,
                size,
            });
        }
        size -= if size > 14. { 2. } else { 1. };
    }
    None
}

/// What is pointed at: a month, and a layer by its year if the pointer is on one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pointed {
    pub month: usize,
    pub year: Option<i32>,
}

/// What is under the point (`x`, `y`) of the picture: the month nearest,
/// and the layer whose outline holds it. Its outline runs straight from
/// month to month, so the edges between are read as it draws them.
pub(crate) fn pointed(drawn: &Drawn, months: u32, x: f32, y: f32) -> Option<Pointed> {
    if months < 2 || drawn.width <= 0. {
        return None;
    }
    let last = months as usize - 1;
    let along = (x / drawn.width * last as f32).clamp(0., last as f32);
    let month = (along.round() as usize).min(last);
    let (left, t) = (along.floor() as usize, along.fract());
    let right = (left + 1).min(last);
    let edge = |edges: &[f32]| {
        let a = edges.get(left).copied().unwrap_or(0.);
        let b = edges.get(right).copied().unwrap_or(a);
        a + (b - a) * t
    };
    let year = drawn
        .layers
        .iter()
        .find(|layer| (edge(&layer.top)..=edge(&layer.bottom)).contains(&y))
        .map(|layer| layer.year);
    Some(Pointed { month, year })
}

/// A list read as a person says it: "A", "A and B", "A, B and C".
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn phrase(text: impl Into<String>, strong: bool) -> Phrase {
    Phrase {
        text: text.into(),
        strong,
    }
}

/// What the picture says, under it: how to read it, or what is pointed at.
pub(crate) fn say(stream: &HistoryStream, pointed: Option<Pointed>) -> Vec<Phrase> {
    let Some(pointed) = pointed else {
        return vec![phrase(
            "Each layer is the artists you first heard in one year, the oldest at the bottom; \
             where one stays thick, what you found then stayed with you. Point at a layer to see who.",
            false,
        )];
    };
    let when = month_name(stream, pointed.month);
    let all = stream.total.get(pointed.month).copied().unwrap_or(0);
    let Some(layer) = stream
        .layers
        .iter()
        .find(|layer| Some(layer.year) == pointed.year)
    else {
        return vec![
            phrase(format!("{when}."), true),
            phrase(format!(" {}.", plural(all, "play")), false),
        ];
    };
    let plays = layer.plays.get(pointed.month).copied().unwrap_or(0);
    let first = stream
        .layers
        .iter()
        .map(|each| each.year)
        .min()
        .unwrap_or(layer.year);
    let share = if all > 0 {
        (plays as f64 / all as f64 * 100.).round() as u64
    } else {
        0
    };
    let names: Vec<String> = layer
        .most
        .get(pointed.month)
        .map(|most| {
            most.iter()
                .map(|&artist| {
                    stream
                        .artists
                        .get(artist as usize)
                        .cloned()
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    let found = if layer.year == first {
        format!("{} and before", layer.year)
    } else {
        layer.year.to_string()
    };
    let mut said = vec![
        phrase(format!("What you found in {found}"), true),
        phrase(
            format!(
                ", in {when}: {}, {share}% of the month.",
                plural(plays, "play")
            ),
            false,
        ),
    ];
    if !names.is_empty() {
        said.push(phrase(format!(" Mostly {}.", and_list(&names)), false));
    }
    said
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ritornel_contract::StreamLayer;

    /// Three years from November 2008: the first year's finds, and the
    /// next's from January 2010.
    fn stream() -> HistoryStream {
        let months = 26;
        let old: Vec<u64> = (0..months).map(|at| if at < 3 { 10 } else { 20 }).collect();
        let found: Vec<u64> = (0..months).map(|at| if at < 14 { 0 } else { 30 }).collect();
        HistoryStream {
            from: Some("2008-11".to_owned()),
            months: months as u32,
            total: old.iter().zip(&found).map(|(a, b)| a + b).collect(),
            layers: vec![
                StreamLayer {
                    year: 2008,
                    most: old.iter().map(|_| vec![0]).collect(),
                    plays: old,
                },
                StreamLayer {
                    year: 2010,
                    most: found
                        .iter()
                        .map(|&plays| if plays > 0 { vec![1, 2, 3] } else { vec![] })
                        .collect(),
                    plays: found,
                },
            ],
            artists: ["Old Friend", "Ada", "Bo", "Cy"]
                .map(str::to_owned)
                .to_vec(),
            days_left_out: 0,
            library_revision: 4,
        }
    }

    fn text(phrases: &[Phrase]) -> String {
        phrases.iter().map(|phrase| phrase.text.as_str()).collect()
    }

    #[test]
    fn names_its_months_and_counts_its_years() {
        assert_eq!(month_name(&stream(), 0), "November 2008");
        assert_eq!(month_name(&stream(), 14), "January 2010");
        assert_eq!(years_of(&stream()), 3);
        assert_eq!(this_year(&stream()), Some(2010));
    }

    #[test]
    fn is_smoothed_and_keeps_what_it_is_given() {
        let smoothed = smooth(&[0., 0., 0., 100., 0., 0., 0.]);
        assert!(smoothed[3] < 100.);
        assert!(smoothed[2] > 0.);
        let even: Vec<f32> = smooth(&[5., 5., 5.]).into_iter().map(f32::round).collect();
        assert_eq!(even, [5., 5., 5.]);
    }

    #[test]
    fn stacks_the_newest_layer_on_top_and_fills_the_stream_between_them() {
        let drawn = draw(&stream(), 1000., 300., 0.);
        let names: Vec<&str> = drawn
            .layers
            .iter()
            .map(|layer| layer.name.as_str())
            .collect();
        assert_eq!(names, ["2010", "2008 and before"]);
        // The newer layer takes more of the hue.
        assert!(drawn.layers[0].shade > drawn.layers[1].shade);
        let points = outline(&drawn.layers[0].top, &drawn.layers[0].bottom, drawn.step);
        assert_eq!(points[0].0, -2.);
        assert_eq!(points[1].0, 0.);
        // A year's name stands under its middle.
        let years: Vec<i32> = drawn.years.iter().map(|mark| mark.year).collect();
        assert_eq!(years, [2009, 2010]);
        assert!((drawn.years[0].x - 8. * 1000. / 25.).abs() < 0.01);
    }

    #[test]
    fn runs_its_months_edge_to_edge_and_keeps_what_is_written_on_it_inside_the_margins() {
        let drawn = draw(&stream(), 1000., 300., 330.);
        // The first month is at the window's edge and the last at the other.
        let points = outline(&drawn.layers[0].top, &drawn.layers[0].bottom, drawn.step);
        assert_eq!((points[0].0, points[1].0), (-2., 0.));
        assert!(points.iter().any(|&(x, _)| (x - 1000.).abs() < 0.01));
        // A year that would stand in the margin is brought inside it.
        let xs: Vec<f32> = drawn.years.iter().map(|mark| mark.x).collect();
        assert_eq!(xs, [348., 652.]);
        for layer in &drawn.layers {
            let Some(label) = layer.label else { continue };
            let half = layer.name.len() as f32 * label.size * 0.285 / 2.;
            assert!(label.x - half >= 330. - 40.);
            assert!(label.x + half <= 670. + 40.);
        }
        // A year the margin moves in nudges its neighbour along, and both stay.
        let long = HistoryStream {
            months: 217,
            total: vec![10; 217],
            layers: Vec::new(),
            ..stream()
        };
        let axis = draw(&long, 1100., 300., 40.).years;
        assert_eq!(axis[0], YearMark { year: 2009, x: 58. });
        let years: Vec<i32> = axis.iter().map(|mark| mark.year).collect();
        assert_eq!(years, (2009..2027).collect::<Vec<_>>());
        assert_eq!(draw(&long, 1440., 300., 40.).years.len(), 18);
        assert_eq!(axis.last().map(|mark| mark.x), Some(1042.));
        assert!(axis.windows(2).all(|pair| pair[1].x - pair[0].x >= 50.));
        assert_eq!(within(5., 18., 1000., 40.), 58.);
        assert_eq!(within(990., 18., 1000., 40.), 942.);
        assert_eq!(within(500., 18., 1000., 40.), 500.);
    }

    #[test]
    fn says_how_to_read_it_and_what_is_pointed_at() {
        assert!(
            text(&say(&stream(), None))
                .starts_with("Each layer is the artists you first heard in one year")
        );
        let at = |month, year| Some(Pointed { month, year });
        assert_eq!(
            text(&say(&stream(), at(20, Some(2010)))),
            "What you found in 2010, in July 2010: 30 plays, 60% of the month. Mostly Ada, Bo and Cy."
        );
        assert_eq!(
            text(&say(&stream(), at(1, Some(2008)))),
            "What you found in 2008 and before, in December 2008: 10 plays, 100% of the month. Mostly Old Friend."
        );
        assert_eq!(
            text(&say(&stream(), at(1, None))),
            "December 2008. 10 plays."
        );
    }

    #[test]
    fn finds_the_layer_under_the_pointer() {
        let drawn = draw(&stream(), 1000., 300., 0.);
        let newest = &drawn.layers[0];
        let x = 20. * drawn.step;
        let y = (newest.top[20] + newest.bottom[20]) / 2.;
        assert_eq!(
            pointed(&drawn, 26, x, y),
            Some(Pointed {
                month: 20,
                year: Some(2010)
            })
        );
        // Above the stream, a month and no layer.
        assert_eq!(
            pointed(&drawn, 26, x, 0.),
            Some(Pointed {
                month: 20,
                year: None
            })
        );
    }
}
