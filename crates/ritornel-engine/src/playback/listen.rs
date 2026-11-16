//! What an open listen has heard: played time counted in frames handed to
//! the output, the ranges of the track they covered, the furthest point
//! reached, and the seeks and pauses. See decisions 19 and 20 and
//! "Recording a listen" in docs/design/playback.md.

use crate::playback::store::{Heard, Started};

/// More ranges than this and neighbours are joined, and the record says it
/// is approximate, which keeps a listen inside one control message.
pub(crate) const MAX_RANGES: usize = 4096;

/// The ranges of a track that were heard, in milliseconds, merged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Ranges {
    ranges: Vec<(i64, i64)>,
    approximate: bool,
}

impl Ranges {
    /// Adds the range from `from` to `to`, merging it with any it touches.
    pub(crate) fn add(&mut self, from: i64, to: i64) {
        if to <= from {
            return;
        }
        let (mut from, mut to) = (from, to);
        self.ranges.retain(|&(start, end)| {
            let touches = start <= to && from <= end;
            if touches {
                from = from.min(start);
                to = to.max(end);
            }
            !touches
        });
        let at = self.ranges.partition_point(|&(start, _)| start < from);
        self.ranges.insert(at, (from, to));
        while self.ranges.len() > MAX_RANGES {
            self.join_closest();
        }
    }

    /// Joins the two neighbours with the smallest gap between them.
    fn join_closest(&mut self) {
        let closest = self
            .ranges
            .windows(2)
            .enumerate()
            .min_by_key(|(_, pair)| match pair {
                [(_, end), (start, _)] => start - end,
                _ => i64::MAX,
            })
            .map(|(at, _)| at);
        if let Some(at) = closest
            && let (Some(&(start, _)), Some(&(_, end))) =
                (self.ranges.get(at), self.ranges.get(at + 1))
        {
            self.ranges.splice(at..at + 2, [(start, end)]);
            self.approximate = true;
        }
    }

    /// The ranges with one more, not yet closed, from `from` to `to`.
    pub(crate) fn with(&self, from: i64, to: i64) -> Self {
        let mut all = self.clone();
        all.add(from, to);
        all
    }

    /// The ranges as the record carries them.
    pub(crate) fn pairs(&self) -> Vec<[i64; 2]> {
        self.ranges.iter().map(|&(from, to)| [from, to]).collect()
    }

    /// Whether neighbours were joined to keep the count down.
    pub(crate) fn approximate(&self) -> bool {
        self.approximate
    }

    /// The ranges as `heard_ranges_json` holds them: `[[from, to], ...]`.
    /// Whether they are approximate is a column of its own beside them.
    pub(crate) fn json(&self) -> String {
        serde_json::to_string(&self.pairs()).unwrap_or_else(|_| "[]".to_owned())
    }

    /// The pairs `heard_ranges_json` holds. Whether they are approximate is
    /// never read from how many there are: ranges joined past the cap can
    /// later merge below it, and the cap's worth of ranges can be exact.
    pub(crate) fn parse(json: &str) -> Vec<[i64; 2]> {
        serde_json::from_str(json).unwrap_or_default()
    }
}

/// A listen that is open: written when its first frame was heard, and
/// closed when the track ends or the player leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenListen {
    pub(crate) started: Started,
    /// The output's rate, which frames are counted at.
    rate: u32,
    /// Frames handed to the output since the listen began.
    frames: u64,
    /// The ranges closed so far.
    ranges: Ranges,
    /// Where the range being heard now began.
    range_from: i64,
    /// Where the track is now.
    position_ms: i64,
    max_position_ms: i64,
    seek_count: i64,
    pause_count: i64,
}

impl OpenListen {
    /// A listen whose first frames have been heard, from `from_ms`.
    pub(crate) fn new(started: Started, rate: u32) -> Self {
        let from = started.start_position_ms;
        Self {
            started,
            rate: rate.max(1),
            frames: 0,
            ranges: Ranges::default(),
            range_from: from,
            position_ms: from,
            max_position_ms: from,
            seek_count: 0,
            pause_count: 0,
        }
    }

    /// More frames were handed to the output, and the track is now at
    /// `position_ms`.
    pub(crate) fn heard(&mut self, frames: u64, position_ms: i64) {
        self.frames += frames;
        self.position_ms = position_ms.max(self.range_from);
        self.max_position_ms = self.max_position_ms.max(self.position_ms);
    }

    /// The listener moved to `to_ms`: the range being heard ends where the
    /// track was, and a new one begins there.
    pub(crate) fn seek(&mut self, to_ms: i64) {
        self.ranges.add(self.range_from, self.position_ms);
        self.range_from = to_ms;
        self.position_ms = to_ms;
        self.seek_count += 1;
    }

    /// The listener paused.
    pub(crate) fn pause(&mut self) {
        self.pause_count += 1;
    }

    /// Where the track is.
    pub(crate) fn position_ms(&self) -> i64 {
        self.position_ms
    }

    /// The played time: frames handed to the output, as milliseconds.
    pub(crate) fn ms_played(&self) -> i64 {
        i64::try_from(self.frames.saturating_mul(1000) / u64::from(self.rate)).unwrap_or(i64::MAX)
    }

    /// The ranges heard, the one being heard included.
    pub(crate) fn ranges(&self) -> Ranges {
        self.ranges.with(self.range_from, self.position_ms)
    }

    /// What a checkpoint saves.
    pub(crate) fn snapshot(&self) -> Heard {
        let ranges = self.ranges();
        Heard {
            ms_played: self.ms_played(),
            ranges_json: ranges.json(),
            ranges_approximate: ranges.approximate(),
            max_position_ms: self.max_position_ms,
            seek_count: self.seek_count,
            pause_count: self.pause_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_merge_where_they_touch_and_keep_their_order() {
        let mut ranges = Ranges::default();
        ranges.add(30_000, 60_000);
        ranges.add(0, 10_000);
        ranges.add(180_000, 192_000);
        ranges.add(10_000, 20_000);
        ranges.add(50_000, 70_000);
        ranges.add(5, 5);
        assert_eq!(
            ranges.pairs(),
            [[0, 20_000], [30_000, 70_000], [180_000, 192_000]]
        );
        assert!(!ranges.approximate());
        assert_eq!(Ranges::parse(&ranges.json()), ranges.pairs());
    }

    #[test]
    fn past_the_limit_the_closest_neighbours_are_joined_and_it_says_so() {
        let mut ranges = Ranges::default();
        for at in 0..=i64::try_from(MAX_RANGES).unwrap() {
            // A gap of 10 between each, but 1 between the first two.
            let from = if at == 1 { 11 } else { at * 20 };
            ranges.add(from, from + 10);
        }
        assert_eq!(ranges.pairs().len(), MAX_RANGES);
        assert!(ranges.approximate());
        assert_eq!(ranges.pairs()[0], [0, 21]);
    }
}
