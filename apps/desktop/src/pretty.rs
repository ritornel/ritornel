// Parts of this file are adapted from WebKit:
// InlineContentConstrainer::prettifyRange
// (Source/WebCore/layout/formattingContexts/inline/InlineContentConstrainer.cpp),
// its constants and its cost, ported to Rust. Those parts stay under
// WebKit's licence, which follows; the rest is under this project's
// (LICENSE).
//
// Copyright (C) 2023 Apple Inc. All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
// 1. Redistributions of source code must retain the above copyright
//    notice, this list of conditions and the following disclaimer.
// 2. Redistributions in binary form must reproduce the above copyright
//    notice, this list of conditions and the following disclaimer in the
//    documentation and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY APPLE INC. AND ITS CONTRIBUTORS ``AS IS''
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
// THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
// PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL APPLE INC. OR ITS CONTRIBUTORS
// BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF
// THE POSSIBILITY OF SUCH DAMAGE.

//! CSS's `text-wrap: pretty`, which the real page sets on its sentences,
//! as WebKit does it (`InlineContentConstrainer::prettifyRange`): the
//! breaks of a paragraph chosen together, Knuth and Plass's way, for the
//! least raggedness against an ideal line a little shorter than the room,
//! rather than each line filled in turn. GPUI fills each line in turn, so
//! the breaks are found here and set as new lines.

/// How far a line may fall short of the ideal or run past it before it
/// costs too much: WebKit's stretchability and shrinkability, 15 points,
/// three times over.
const GIVE: f32 = 15.;
const MOST: f32 = 3.;
/// The last breaks are free of the bounds, so that a short last line does
/// not leave a paragraph with no breaks at all (WebKit's
/// `lastLineBreakingPointOffset`).
const NEAR_END: usize = 5;

/// Knuth and Plass's raggedness of a line `width` wide against `ideal`.
fn raggedness(width: f32, ideal: f32) -> f32 {
    100. * ((width - ideal) / GIVE).powi(3).abs()
}

/// Where a paragraph of words `widths` wide, with `space` between them,
/// breaks across `room`: the words that begin each line after the first.
/// `None` when it fits on one line, or no breaks are good enough: then it
/// is filled line by line, as WebKit falls back to.
pub(crate) fn breaks(widths: &[f32], space: f32, room: f32) -> Option<Vec<usize>> {
    let words = widths.len();
    if words < 2 {
        return None;
    }
    // Room for a rounding of the shaped widths, as a line just fits.
    let room = room + 0.5;
    let line = |from: usize, to: usize| -> f32 {
        widths.get(from..to).map_or(f32::INFINITY, |run| {
            run.iter().sum::<f32>() + space * run.len().saturating_sub(1) as f32
        })
    };
    if line(0, words) <= room {
        return None;
    }
    let ideal = room - GIVE * MOST;
    if widths.iter().copied().fold(0., f32::max) > ideal {
        return None;
    }
    // best[j]: the least cost of the lines before word j, and where the
    // last of them began. Word `words` is the end.
    let mut best: Vec<(f32, usize)> = vec![(f32::INFINITY, 0); words + 1];
    if let Some(first) = best.first_mut() {
        *first = (0., 0);
    }
    for end in 1..=words {
        for start in (0..end).rev() {
            let width = line(start, end);
            if width > room {
                break;
            }
            let Some(&(before, _)) = best.get(start) else {
                continue;
            };
            if before.is_infinite() {
                continue;
            }
            let bounded = end + NEAR_END < words + 1;
            if bounded && (width - ideal).abs() > GIVE * MOST {
                continue;
            }
            let cost = before + raggedness(width, ideal);
            if let Some(entry) = best.get_mut(end)
                && cost < entry.0
            {
                *entry = (cost, start);
            }
        }
    }
    let mut starts = Vec::new();
    let mut at = words;
    while at > 0 {
        let &(cost, start) = best.get(at)?;
        if cost.is_infinite() || start >= at {
            return None;
        }
        if start > 0 {
            starts.push(start);
        }
        at = start;
    }
    starts.reverse();
    Some(starts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_a_line_that_fits_alone() {
        assert_eq!(breaks(&[50., 50.], 5., 200.), None);
    }

    #[test]
    fn evens_the_lines_out_rather_than_filling_each() {
        // Filled in turn, eleven words of 50 across 300 make lines of 5, 5
        // and 1, the last word alone; evened out, none stands alone.
        let widths = [50.; 11];
        let starts = breaks(&widths, 5., 300.).unwrap_or_default();
        assert_eq!(starts.len(), 2);
        assert!(starts.last().is_some_and(|&last| 11 - last >= 3));
    }

    #[test]
    fn breaks_the_caption_under_history_as_webkit_does() {
        // The words of History's caption at 18 points, as GPUI measures
        // them. Across the 621 points WebKit gives it beside a scroll bar,
        // the real page breaks after "the" and before "Point".
        let widths = [
            39.39, 38.28, 12.91, 25.96, 49.69, 29.10, 31.09, 46.01, 13.99, 29.96, 38.25, 25.96,
            49.48, 15.50, 25.96, 62.82, 49.24, 29.96, 41.98, 43.84, 39.06, 29.10, 46.36, 35.97,
            53.31, 33.59, 33.94, 40.86, 15.50, 9.45, 38.28, 16.06, 28.53, 38.35,
        ];
        assert_eq!(breaks(&widths, 4.56, 621.), Some(vec![15, 27]));
    }
}
