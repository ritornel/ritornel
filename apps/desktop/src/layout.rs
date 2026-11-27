//! Where everything on a wall goes: tiles of 150 points, at least 18 apart
//! across and 22 down; under letters, a lane of 96 points for the large
//! letter and 40 between one letter's tiles and the next's; with the large
//! letters off, a small heading over each letter's tiles; under a date, a
//! heading of 52 points over each stretch's tiles; flat, nothing over them
//! at all. The room left over goes between the columns.
//!
//! A wall need not be of tiles (`Metrics`): tiles may be smaller or larger
//! than 150 points, and a list or a table is one row to a line, as wide as
//! the wall, with nothing between the rows.

/// How a wall is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// Under letters in a lane.
    Lettered,
    /// Under small headings of letters, with the large letters turned off.
    Headed,
    /// Under headings of time, or of anything else that is not a letter.
    Dated,
    /// Under nothing: the rows in one run.
    Flat,
}

/// What a wall's rows measure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Metrics {
    /// Tiles `tile` points wide, as many as fit across, their words under
    /// them.
    Grid { tile: f32 },
    /// One row to a line, `row` points high, with nothing between them.
    Rows { row: f32 },
}

impl Metrics {
    /// The wall of tiles the Library has always had.
    #[cfg(test)]
    pub(crate) const WALL: Self = Self::Grid { tile: TILE };
}

pub(crate) const LANE: f32 = 96.;
pub(crate) const GAP_X: f32 = 18.;
pub(crate) const GAP_Y: f32 = 22.;
pub(crate) const TILE: f32 = 150.;
pub(crate) const MAX_COLUMNS: usize = 16;
pub(crate) const MIN_COLUMNS: usize = 3;
/// Under a tile's art: 9 points, a 20-point title and an 18-point line.
pub(crate) const TEXT_HEIGHT: f32 = 9. + 20. + 18.;
pub(crate) const GROUP_GAP: f32 = 40.;
/// A heading of time: 12 above, a 30-point line and 10 below, then 4 to the tiles.
pub(crate) const HEADING: f32 = 12. + 30. + 10.;
pub(crate) const HEADING_GAP: f32 = 4.;
pub(crate) const PERIOD_GAP: f32 = 34.;
/// A small heading of a letter: 6 above, a 22-point line and 8 below.
pub(crate) const LETTER_HEADING: f32 = 6. + 22. + 8.;
pub(crate) const LETTER_GAP: f32 = 22.;

impl Shape {
    fn gap_above(self) -> f32 {
        match self {
            Self::Lettered => GROUP_GAP,
            Self::Headed => LETTER_GAP,
            Self::Dated => PERIOD_GAP,
            Self::Flat => 0.,
        }
    }

    fn head(self) -> f32 {
        match self {
            Self::Lettered | Self::Flat => 0.,
            Self::Headed => LETTER_HEADING,
            Self::Dated => HEADING + HEADING_GAP,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Group {
    pub label: String,
    /// The index of its first row in the view.
    pub start: usize,
    pub count: usize,
    /// Where it begins on the wall.
    pub top: f32,
    /// Where its tiles begin, from its top.
    pub body_top: f32,
    pub lines: usize,
    /// Its height, tiles and heading together.
    pub height: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub shape: Shape,
    pub metrics: Metrics,
    pub width: f32,
    /// How wide a tile is, or a row.
    pub tile: f32,
    pub columns: usize,
    pub row_height: f32,
    pub pitch: f32,
    /// Where the tiles start across: after the lane, or at the edge.
    pub left: f32,
    /// The space between columns.
    pub gap: f32,
    pub groups: Vec<Group>,
    pub height: f32,
    pub total: usize,
}

/// How many tiles of 150 points fit across `width`.
#[cfg(test)]
pub(crate) fn columns_for(width: f32, shape: Shape) -> usize {
    columns_of(width, shape, TILE)
}

/// How many tiles `tile` points wide fit across `width`.
fn columns_of(width: f32, shape: Shape, tile: f32) -> usize {
    let room = width - if shape == Shape::Lettered { LANE } else { 0. };
    let fit = ((room + GAP_X) / (tile + GAP_X)).floor().max(0.) as usize;
    fit.clamp(MIN_COLUMNS, MAX_COLUMNS)
}

/// Lays out a wall of 150-point tiles, as [`layout_of`] does.
#[cfg(test)]
pub(crate) fn layout(
    width: f32,
    total: usize,
    sections: &[(String, usize)],
    shape: Shape,
) -> Layout {
    layout_of(width, total, sections, shape, Metrics::WALL)
}

/// Lays out a wall `width` points wide holding `total` rows in sections
/// that start at these indices. With no sections, or flat, the rows are
/// one group.
pub(crate) fn layout_of(
    width: f32,
    total: usize,
    sections: &[(String, usize)],
    shape: Shape,
    metrics: Metrics,
) -> Layout {
    let left = if shape == Shape::Lettered { LANE } else { 0. };
    let room = width - left;
    let (columns, tile, row_height, gap_y) = match metrics {
        Metrics::Grid { tile } => (
            columns_of(width, shape, tile),
            tile,
            tile + TEXT_HEIGHT,
            GAP_Y,
        ),
        Metrics::Rows { row } => (1, room.max(0.), row, 0.),
    };
    let pitch = row_height + gap_y;
    let gap = match metrics {
        Metrics::Grid { .. } if columns > 1 => {
            ((room - columns as f32 * tile) / (columns - 1) as f32).max(GAP_X)
        }
        Metrics::Grid { .. } => GAP_X,
        Metrics::Rows { .. } => 0.,
    };
    let fallback = [(String::new(), 0)];
    let starts: &[(String, usize)] = if !sections.is_empty() && shape != Shape::Flat {
        sections
    } else if total > 0 {
        &fallback
    } else {
        &[]
    };
    let mut groups: Vec<Group> = Vec::new();
    let mut y = 0.;
    for (at, (label, start)) in starts.iter().enumerate() {
        let end = starts.get(at + 1).map_or(total, |next| next.1);
        let count = end.saturating_sub(*start);
        if count == 0 {
            continue;
        }
        let lines = count.div_ceil(columns);
        let tiles = lines as f32 * row_height + (lines - 1) as f32 * gap_y;
        if !groups.is_empty() {
            y += shape.gap_above();
        }
        let body_top = shape.head();
        groups.push(Group {
            label: label.clone(),
            start: *start,
            count,
            top: y,
            body_top,
            lines,
            height: body_top + tiles,
        });
        y += body_top + tiles;
    }
    Layout {
        shape,
        metrics,
        width,
        tile,
        columns,
        row_height,
        pitch,
        left,
        gap,
        groups,
        height: y,
        total,
    }
}

/// One line of tiles.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Line {
    /// Where it is on the wall.
    pub y: f32,
    pub first: usize,
    pub count: usize,
}

/// A step an arrow key takes across a wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Left,
    Right,
    Up,
    Down,
}

impl Layout {
    /// The lines of tiles between `top` and `bottom` on the wall.
    pub(crate) fn lines_between(&self, top: f32, bottom: f32) -> Vec<Line> {
        let mut lines = Vec::new();
        let from = self
            .groups
            .partition_point(|group| group.top + group.height < top);
        for group in self.groups.iter().skip(from) {
            if group.top > bottom {
                break;
            }
            let body_top = group.top + group.body_top;
            let first_line = ((top - body_top) / self.pitch).floor().max(0.) as usize;
            let last_line =
                (((bottom - body_top) / self.pitch).floor().max(0.) as usize).min(group.lines - 1);
            for line in first_line..=last_line {
                let first = group.start + line * self.columns;
                lines.push(Line {
                    y: body_top + line as f32 * self.pitch,
                    first,
                    count: self.columns.min(group.start + group.count - first),
                });
            }
        }
        lines
    }

    /// Where a tile is on the wall: its left, from the wall's left edge, and its top.
    pub(crate) fn tile_origin(&self, line: &Line, index: usize) -> (f32, f32) {
        let column = index - line.first;
        (self.left + column as f32 * (self.tile + self.gap), line.y)
    }

    pub(crate) fn group_of(&self, index: usize) -> Option<&Group> {
        let at = self
            .groups
            .partition_point(|group| group.start + group.count <= index);
        self.groups.get(at).filter(|group| index >= group.start)
    }

    /// Where the line holding row `index` begins on the wall.
    pub(crate) fn y_of(&self, index: usize) -> f32 {
        let Some(group) = self.group_of(index) else {
            return 0.;
        };
        let line = (index - group.start) / self.columns;
        group.top + group.body_top + line as f32 * self.pitch
    }

    /// Where row `index`'s tile is on the wall: its left and its top.
    pub(crate) fn origin_of(&self, index: usize) -> Option<(f32, f32)> {
        let group = self.group_of(index)?;
        let column = (index - group.start) % self.columns;
        Some((
            self.left + column as f32 * (self.tile + self.gap),
            self.y_of(index),
        ))
    }

    /// The first row whose tiles are at or below `y`.
    pub(crate) fn index_at(&self, y: f32) -> usize {
        for group in &self.groups {
            if y >= group.top + group.height {
                continue;
            }
            let line = ((y - group.top - group.body_top) / self.pitch)
                .floor()
                .max(0.) as usize;
            return (group.start + line * self.columns).min(group.start + group.count - 1);
        }
        self.total.saturating_sub(1)
    }

    /// Where an arrow key goes from the tile at `from`: ← and → to the row
    /// before or after in the view; ↑ and ↓ a line in the same column,
    /// across a section to the nearest line of the next, at the end of a
    /// shorter one. At either end of the wall a step goes nowhere.
    pub(crate) fn step_from(&self, from: usize, step: Step) -> usize {
        let last = self.total.saturating_sub(1);
        let at = from.min(last);
        match step {
            Step::Left => return at.saturating_sub(1),
            Step::Right => return (at + 1).min(last),
            _ => {}
        }
        let Some(which) = self
            .groups
            .iter()
            .position(|group| at >= group.start && at < group.start + group.count)
        else {
            return at;
        };
        let Some(group) = self.groups.get(which) else {
            return at;
        };
        let columns = self.columns;
        let line = (at - group.start) / columns;
        let column = (at - group.start) % columns;
        let end = |each: &Group| each.start + each.count - 1;
        if step == Step::Down {
            if line + 1 < group.lines {
                return end(group).min(at + columns);
            }
            return self
                .groups
                .get(which + 1)
                .map_or(at, |next| end(next).min(next.start + column));
        }
        if line > 0 {
            return at - columns;
        }
        match which
            .checked_sub(1)
            .and_then(|before| self.groups.get(before))
        {
            Some(before) => end(before).min(before.start + (before.lines - 1) * columns + column),
            None => at,
        }
    }

    /// The tile an arrow selects when nothing is: the first of the first
    /// line wholly below `top` that starts above `bottom`, or of the line at
    /// `top` when there is none. `None` for an empty wall.
    pub(crate) fn first_in_view(&self, top: f32, bottom: f32) -> Option<usize> {
        if self.total == 0 {
            return None;
        }
        let lines = self.lines_between(top, bottom);
        let whole = lines.iter().find(|line| line.y >= top && line.y < bottom);
        Some(
            whole
                .or(lines.first())
                .map_or_else(|| self.index_at(top.max(0.)), |line| line.first),
        )
    }

    /// Where the wall's top must be for the line holding row `index` to be
    /// in view, `margin` clear of the window's edges and of `above` that
    /// covers its top: the least scroll that does it, or `None` when it is
    /// in view already.
    pub(crate) fn scroll_to_show(
        &self,
        index: usize,
        top: f32,
        height: f32,
        above: f32,
        margin: f32,
    ) -> Option<f32> {
        let y = self.y_of(index);
        if y - margin < top + above {
            return Some(y - margin - above);
        }
        let bottom = y + self.row_height + margin;
        (bottom > top + height).then_some(bottom - height)
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn letters() -> Vec<(String, usize)> {
        vec![
            ("A".to_owned(), 0),
            ("B".to_owned(), 7),
            ("C".to_owned(), 20),
        ]
    }

    #[test]
    fn as_many_tiles_as_fit_and_the_lane_is_left_for_the_letter() {
        assert_eq!(columns_for(1100. - 80., Shape::Lettered), 5);
        assert_eq!(columns_for(1100. - 80., Shape::Dated), 6);
        assert_eq!(columns_for(2000., Shape::Lettered), 11);
        assert_eq!(columns_for(2000., Shape::Dated), 12);
        assert_eq!(columns_for(300., Shape::Lettered), 3);
        assert_eq!(columns_for(9000., Shape::Dated), 16);
    }

    #[test]
    fn groups_stack_with_their_gaps_and_rows_are_found_by_place() {
        let wall = layout(1020., 25, &letters(), Shape::Lettered);
        assert_eq!(wall.columns, 5);
        let (a, b, c) = (&wall.groups[0], &wall.groups[1], &wall.groups[2]);
        assert_eq!((a.start, a.count, a.lines), (0, 7, 2));
        assert_eq!(b.top, a.height + GROUP_GAP);
        assert_eq!(c.top, b.top + b.height + GROUP_GAP);
        assert_eq!(wall.height, c.top + c.height);
        assert_eq!(wall.y_of(6), wall.pitch);
        assert_eq!(wall.y_of(7), b.top);
        assert_eq!(wall.index_at(wall.y_of(12) + 1.), 12);
        assert_eq!(wall.index_at(wall.height + 500.), 24);
        assert_eq!(
            wall.group_of(19).map(|group| group.label.as_str()),
            Some("B")
        );
    }

    #[test]
    fn headed_and_dated_walls_put_a_heading_over_each_group() {
        let headed = layout(1020., 25, &letters(), Shape::Headed);
        assert_eq!(headed.left, 0.);
        assert_eq!(headed.columns, 6);
        assert_eq!(headed.groups[0].body_top, LETTER_HEADING);
        assert_eq!(headed.groups[1].top, headed.groups[0].height + LETTER_GAP);
        assert_eq!(headed.y_of(7), headed.groups[1].top + LETTER_HEADING);
        let dated = layout(1020., 25, &letters(), Shape::Dated);
        assert_eq!(dated.groups[0].body_top, HEADING + HEADING_GAP);
        assert_eq!(dated.groups[1].top, dated.groups[0].height + PERIOD_GAP);
        assert!(layout(1020., 0, &[], Shape::Lettered).groups.is_empty());
    }

    #[test]
    fn a_flat_wall_is_one_group_with_nothing_over_it() {
        let flat = layout_of(1020., 25, &[], Shape::Flat, Metrics::WALL);
        assert_eq!((flat.left, flat.columns, flat.groups.len()), (0., 6, 1));
        let only = &flat.groups[0];
        assert_eq!(
            (only.start, only.count, only.body_top, only.top),
            (0, 25, 0., 0.)
        );
        assert_eq!(only.lines, 5);
        assert_eq!(flat.height, 5. * flat.row_height + 4. * GAP_Y);
        assert_eq!(flat.y_of(6), flat.pitch);
        // Sections it is given are not drawn: it is still one run.
        let run = layout_of(1020., 25, &letters(), Shape::Flat, Metrics::WALL);
        assert_eq!(run.groups.len(), 1);
        assert_eq!(run.height, flat.height);
        assert!(
            layout_of(1020., 0, &[], Shape::Flat, Metrics::WALL)
                .groups
                .is_empty()
        );
    }

    #[test]
    fn smaller_tiles_fit_more_across_and_larger_fewer() {
        let wide = |tile: f32| layout_of(1020., 60, &[], Shape::Flat, Metrics::Grid { tile });
        let (compact, comfortable, roomy) = (wide(124.), wide(150.), wide(184.));
        assert_eq!(
            (compact.columns, comfortable.columns, roomy.columns),
            (7, 6, 5)
        );
        for wall in [&compact, &comfortable, &roomy] {
            // The tiles reach both edges, the room left over between them.
            let last = wall.columns - 1;
            let right = wall.origin_of(last).map(|(x, _)| x + wall.tile);
            assert!(
                right.is_some_and(|right| (right - 1020.).abs() < 0.01),
                "{wall:?}"
            );
            assert_eq!(wall.row_height, wall.tile + TEXT_HEIGHT);
            assert!(wall.gap >= GAP_X);
        }
        assert_eq!(compact.tile, 124.);
        // Beside the letters' lane there is less room.
        let lettered = layout_of(
            1020.,
            25,
            &letters(),
            Shape::Lettered,
            Metrics::Grid { tile: 184. },
        );
        assert_eq!((lettered.left, lettered.columns), (LANE, 4));
    }

    #[test]
    fn a_list_or_a_table_is_one_row_to_a_line_with_nothing_between() {
        let rows = Metrics::Rows { row: 36. };
        let table = layout_of(1020., 25, &[], Shape::Flat, rows);
        assert_eq!((table.columns, table.tile, table.gap), (1, 1020., 0.));
        assert_eq!((table.row_height, table.pitch), (36., 36.));
        assert_eq!(table.height, 25. * 36.);
        assert_eq!(table.y_of(10), 360.);
        assert_eq!(table.origin_of(10), Some((0., 360.)));
        assert_eq!(table.index_at(361.), 10);
        let lines = table.lines_between(100., 200.);
        assert_eq!(
            lines
                .iter()
                .map(|line| (line.first, line.count))
                .collect::<Vec<_>>(),
            [(2, 1), (3, 1), (4, 1), (5, 1)]
        );
        // Every arrow but ↑ and ↓ would stay in its line: here they all
        // move a row.
        use Step::*;
        assert_eq!(
            [
                table.step_from(3, Down),
                table.step_from(3, Up),
                table.step_from(3, Right),
                table.step_from(0, Up),
                table.step_from(24, Down)
            ],
            [4, 2, 4, 0, 24]
        );
        assert_eq!(table.first_in_view(1., 400.), Some(1));
        assert_eq!(
            table.scroll_to_show(20, 0., 360., 0., 0.),
            Some(21. * 36. - 360.)
        );

        // Under headings each group has its own, the rows running on under
        // it, and under letters the rows start after the lane.
        let headed = layout_of(1020., 25, &letters(), Shape::Headed, rows);
        let (a, b) = (&headed.groups[0], &headed.groups[1]);
        assert_eq!(a.height, LETTER_HEADING + 7. * 36.);
        assert_eq!(b.top, a.height + LETTER_GAP);
        assert_eq!(headed.y_of(7), b.top + LETTER_HEADING);
        assert_eq!(headed.step_from(6, Down), 7);
        assert_eq!(headed.step_from(7, Up), 6);
        let lettered = layout_of(1020., 25, &letters(), Shape::Lettered, rows);
        assert_eq!((lettered.left, lettered.tile), (LANE, 1020. - LANE));
        assert_eq!(
            lettered.origin_of(8),
            Some((LANE, lettered.groups[1].top + 36.))
        );
        // A denser list is the same list, closer.
        let dense = layout_of(1020., 25, &[], Shape::Flat, Metrics::Rows { row: 30. });
        assert_eq!(dense.height, 25. * 30.);
    }

    #[test]
    fn the_arrow_keys_move_as_the_real_wall_does() {
        // Five to a line: A is 0–4 and 5–6, B is 7–11, 12–16 and 17–19, C is 20–24.
        let wall = layout(1020., 25, &letters(), Shape::Lettered);
        let go = |from: usize, steps: &[Step]| {
            steps
                .iter()
                .fold(from, |at, step| wall.step_from(at, *step))
        };
        use Step::*;
        assert_eq!(
            [
                go(2, &[Right]),
                go(4, &[Right]),
                go(6, &[Right]),
                go(7, &[Left]),
                go(5, &[Left])
            ],
            [3, 5, 7, 6, 4]
        );
        assert_eq!(
            [
                go(1, &[Down]),
                go(8, &[Down]),
                go(13, &[Up]),
                go(13, &[Down, Up])
            ],
            [6, 13, 8, 13]
        );
        assert_eq!([go(3, &[Down]), go(14, &[Down])], [6, 19]);
        assert_eq!(
            [
                go(5, &[Down]),
                go(6, &[Down]),
                go(19, &[Down]),
                go(8, &[Up]),
                go(12, &[Up]),
                go(20, &[Up])
            ],
            [7, 8, 22, 6, 7, 17]
        );
        assert_eq!([go(10, &[Up]), go(24, &[Up])], [6, 19]);
        assert_eq!(
            [
                go(0, &[Left]),
                go(2, &[Up]),
                go(24, &[Right]),
                go(22, &[Down]),
                go(40, &[Left])
            ],
            [0, 2, 24, 22, 23]
        );
        let one = layout(1020., 12, &[], Shape::Lettered);
        assert_eq!(
            [
                one.step_from(9, Down),
                one.step_from(11, Down),
                one.step_from(3, Up),
                one.step_from(11, Up)
            ],
            [11, 11, 3, 6]
        );
    }

    #[test]
    fn with_nothing_selected_the_first_tile_wholly_in_view_is() {
        let wall = layout(1020., 25, &letters(), Shape::Lettered);
        assert_eq!(wall.first_in_view(-120., 600.), Some(0));
        assert_eq!(wall.first_in_view(1., 600.), Some(5));
        assert_eq!(wall.first_in_view(wall.groups[1].top - 10., 900.), Some(7));
        assert_eq!(
            wall.first_in_view(wall.groups[2].top + 10., wall.groups[2].top + 60.),
            Some(20)
        );
        assert_eq!(
            layout(1020., 0, &[], Shape::Lettered).first_in_view(0., 600.),
            None
        );
    }

    #[test]
    fn scrolls_only_as_far_as_keeps_the_line_in_view() {
        let wall = layout(1020., 25, &letters(), Shape::Lettered);
        let margin = GAP_Y;
        assert_eq!(wall.scroll_to_show(8, -100., 900., 0., margin), None);
        let top = wall.y_of(7) + 50.;
        assert_eq!(
            wall.scroll_to_show(7, top, 600., 0., margin),
            Some(wall.y_of(7) - margin)
        );
        assert_eq!(
            wall.scroll_to_show(7, top, 600., 56., margin),
            Some(wall.y_of(7) - margin - 56.)
        );
    }
}
