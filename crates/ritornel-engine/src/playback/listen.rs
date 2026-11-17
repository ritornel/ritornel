//! The listen recorder: each listen, folded from what the output says it
//! did, with no I/O. A span that opens a listen opens it when its first
//! frame is heard, dated then; each span's end adds its frames and the
//! range of the track they covered; and the listen closes, dated when its
//! last frame was, unless a seek's span carries it on. Played time is
//! counted in frames handed to the output. See decisions 19 and 20 and
//! "Recording a listen" in docs/design/playback.md.

use std::collections::HashMap;

use ritornel_domain::history::EndReason;

use crate::Id;
use crate::playback::renderer::{End, Event, Progress};
use crate::playback::store::{Ended, Heard, Saved, Started};

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

/// What the player says of a span as it hands it down, before the output
/// can say anything of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Span {
    /// The listen it opens as its first frame is heard: all of it but when,
    /// decided with the span.
    pub(crate) listen: Started,
    /// The listen it carries on, where that is still open as the span
    /// begins: a seek's.
    pub(crate) continues: Option<Id>,
    /// Where in the track it starts.
    pub(crate) from_ms: i64,
    /// The output's rate, which its frames are counted at.
    pub(crate) rate: u32,
    /// Why its listen ends if the span is replaced: noted by the command
    /// that replaces it.
    pub(crate) end: Option<EndReason>,
}

/// A listen: open from its first frame heard until its track ends or the
/// player leaves it, and then closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listen {
    pub(crate) started: Started,
    /// The output's rate, which frames are counted at.
    rate: u32,
    /// Frames of its spans that have ended.
    frames: u64,
    /// The ranges closed so far.
    ranges: Ranges,
    /// Where the range being heard now began, and where it has reached.
    range_from: i64,
    position_ms: i64,
    max_position_ms: i64,
    seek_count: i64,
    pause_count: i64,
    /// The span playing it now.
    live: Option<u64>,
    pub(crate) ended: Option<Ended>,
}

impl Listen {
    /// A listen whose first frame has just been heard.
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
            live: None,
            ended: None,
        }
    }

    fn ms(&self, frames: u64) -> i64 {
        i64::try_from(frames.saturating_mul(1000) / u64::from(self.rate)).unwrap_or(i64::MAX)
    }

    /// Where the track is after `frames` more of the range being heard.
    fn reached(&self, frames: u64) -> i64 {
        let end = self.started.track_duration_ms.max(self.range_from);
        self.range_from.saturating_add(self.ms(frames)).min(end)
    }

    /// The range being heard ends, `frames` into it.
    pub(crate) fn heard(&mut self, frames: u64) {
        self.frames += frames;
        self.position_ms = self.reached(frames);
        self.max_position_ms = self.max_position_ms.max(self.position_ms);
        self.ranges.add(self.range_from, self.position_ms);
        self.range_from = self.position_ms;
    }

    /// The listener moved to `to_ms`.
    pub(crate) fn seek(&mut self, to_ms: i64) {
        (self.range_from, self.position_ms) = (to_ms, to_ms);
        self.seek_count += 1;
    }

    /// The played time, with `live` frames of the span playing it.
    pub(crate) fn ms_played(&self, live: u64) -> i64 {
        self.ms(self.frames + live)
    }

    /// Where the track is, with `live` frames of the span playing it.
    pub(crate) fn position_ms(&self, live: u64) -> i64 {
        if self.live.is_some() {
            self.reached(live)
        } else {
            self.position_ms
        }
    }

    /// What a save writes, with `live` frames of the span playing it.
    pub(crate) fn snapshot(&self, live: u64) -> Heard {
        let position = self.position_ms(live);
        let ranges = self.ranges.with(self.range_from, position);
        Heard {
            ms_played: self.ms_played(live),
            ranges_json: ranges.json(),
            ranges_approximate: ranges.approximate(),
            max_position_ms: self.max_position_ms.max(position),
            seek_count: self.seek_count,
            pause_count: self.pause_count,
        }
    }
}

/// Every listen, folded from what the output says it did, with no I/O:
/// the spans handed down that have not ended, the listen open, and those
/// closed and not yet saved.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    spans: HashMap<u64, Span>,
    open: Option<Listen>,
    closed: Vec<Listen>,
    /// Whether the open listen changed in a way its frames do not show
    /// since it was saved: it opened, or was sought or paused.
    changed: bool,
    /// What the open listen had played when it was last saved, and whether
    /// it has not been since it opened: its saves run every so often from
    /// its first frame.
    saved_ms: i64,
    fresh: bool,
    /// The listen closed last, and why.
    last: Option<(Id, EndReason)>,
}

impl Recorder {
    /// Says what span `span` is, before any of it can be heard.
    pub(crate) fn register(&mut self, span: u64, spec: Span) {
        self.spans.insert(span, spec);
    }

    /// Notes why the listen of each span handed down ends, if the command
    /// about to replace them does.
    pub(crate) fn note(&mut self, reason: EndReason) {
        for spec in self.spans.values_mut() {
            spec.end = Some(reason);
        }
    }

    /// The listen open, if one is.
    pub(crate) fn open(&self) -> Option<&Listen> {
        self.open.as_ref()
    }

    /// The listen closed last, and why.
    pub(crate) fn last_closed(&self) -> Option<(Id, EndReason)> {
        self.last
    }

    /// The frames of `progress` that the open listen has not been told of.
    pub(crate) fn live(&self, progress: Progress) -> u64 {
        let playing = self.open.as_ref().and_then(|listen| listen.live);
        if playing == Some(progress.span) {
            progress.frames
        } else {
            0
        }
    }

    /// The listener paused.
    pub(crate) fn paused(&mut self) {
        if let Some(listen) = &mut self.open {
            listen.pause_count += 1;
            self.changed = true;
        }
    }

    /// Takes in one thing the output did. `time` gives an output time as
    /// the wall and monotonic clocks read it.
    pub(crate) fn fold(&mut self, event: &Event, time: &dyn Fn(u64) -> (i64, i64)) {
        match *event {
            Event::Began { span, at } => self.began(span, time(at)),
            Event::Ended {
                span,
                frames,
                at,
                ref end,
            } => self.ended(span, frames, end, time(at)),
        }
    }

    fn began(&mut self, span: u64, (wall, mono): (i64, i64)) {
        let Some(spec) = self.spans.get(&span).cloned() else {
            return;
        };
        if let Some(open) = &mut self.open {
            if open.live.is_none() && spec.continues == Some(open.started.listen_id) {
                open.live = Some(span);
                return;
            }
            // A span begins only once the one before has ended; were one
            // still open, it ends here.
            self.close(EndReason::Stopped, (wall, mono));
        }
        let started = Started {
            started_at_ms: wall,
            started_mono_ms: mono,
            start_position_ms: spec.from_ms,
            ..spec.listen.clone()
        };
        let mut listen = Listen::new(started, spec.rate);
        listen.live = Some(span);
        self.open = Some(listen);
        (self.changed, self.fresh, self.saved_ms) = (true, true, 0);
    }

    fn ended(&mut self, span: u64, frames: u64, end: &End, at: (i64, i64)) {
        let Some(spec) = self.spans.remove(&span) else {
            return;
        };
        let Some(open) = &mut self.open else {
            return;
        };
        let id = open.started.listen_id;
        if open.live == Some(span) {
            open.heard(frames);
            open.live = None;
        } else if open.live.is_some() || spec.continues != Some(id) {
            // Not its listen's: a span that was never heard.
            return;
        }
        // Replaced by a seek, it carries on in the span that continues it.
        let carried = matches!(end, End::Replaced)
            .then(|| {
                self.spans
                    .iter()
                    .filter(|(_, spec)| spec.continues == Some(id))
                    .max_by_key(|(span, _)| **span)
                    .map(|(_, spec)| spec.from_ms)
            })
            .flatten();
        if let Some(to) = carried {
            open.seek(to);
            self.changed = true;
            return;
        }
        let reason = match end {
            End::Joined { .. } | End::Drained => EndReason::Finished,
            End::Failed(_) => EndReason::Error,
            End::Replaced => spec.end.unwrap_or(EndReason::Stopped),
        };
        self.close(reason, at);
    }

    fn close(&mut self, reason: EndReason, (wall, mono): (i64, i64)) {
        if let Some(mut listen) = self.open.take() {
            listen.ended = Some(Ended {
                end_position_ms: listen.position_ms,
                end_reason: reason,
                ended_at_ms: wall,
                ended_mono_ms: Some(mono),
            });
            self.last = Some((listen.started.listen_id, reason));
            self.closed.push(listen);
        }
    }

    /// Whether a save is due: a listen opened or closed, the open one was
    /// sought or paused, or it has played `every_ms` since it was saved.
    pub(crate) fn due(&self, live: u64, every_ms: i64) -> bool {
        !self.closed.is_empty()
            || self.changed
            || self
                .open
                .as_ref()
                .is_some_and(|listen| listen.ms_played(live) - self.saved_ms >= every_ms)
    }

    /// What a save writes: each listen closed since the last, oldest first,
    /// then the open one, with `live` frames of the span playing it.
    pub(crate) fn unsaved(&self, live: u64) -> Vec<Saved> {
        let closed = self.closed.iter().map(|listen| Saved {
            started: listen.started.clone(),
            heard: listen.snapshot(0),
            position_ms: listen.position_ms,
            ended: listen.ended.clone(),
        });
        let open = self.open.iter().map(|listen| Saved {
            started: listen.started.clone(),
            heard: listen.snapshot(live),
            position_ms: listen.position_ms(live),
            ended: None,
        });
        closed.chain(open).collect()
    }

    /// The save of [`unsaved`](Self::unsaved) committed. Whether it closed
    /// any listen.
    pub(crate) fn saved(&mut self, live: u64) -> bool {
        if let Some(open) = self.open.as_ref().filter(|_| !self.fresh) {
            self.saved_ms = open.ms_played(live);
        }
        (self.changed, self.fresh) = (false, false);
        !std::mem::take(&mut self.closed).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Draws;

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

    fn started(listen: u8) -> Started {
        let id = Id::from_bytes([listen; 16]);
        Started {
            listen_id: id,
            entry: None,
            listener: id,
            actor_session: None,
            played_on_server: id,
            appearance: id,
            asset: id,
            source_item: id,
            source_revision: 1,
            started_at_ms: 0,
            zone: None,
            started_mono_ms: 0,
            boot: id,
            start_reason: ritornel_domain::history::StartReason::UserPick,
            continues: None,
            start_position_ms: 0,
            track_duration_ms: 1_000_000,
            context_kind: ritornel_domain::history::ContextKind::Manual,
            context_ref: None,
            shuffle: false,
        }
    }

    #[test]
    fn whatever_the_output_says_every_listen_closes_once_and_its_time_is_its_frames() {
        let mut sought = 0;
        for seed in 1..=500 {
            let mut draws = Draws::seeded(seed);
            let mut recorder = Recorder::default();
            let time = |at: u64| (i64::try_from(at).unwrap(), i64::try_from(at).unwrap());
            let say = |recorder: &mut Recorder, event: Event| recorder.fold(&event, &time);
            let (mut next, mut clock, mut heard) = (1_u64, 0_u64, 0_u64);
            // The span in hand: its number, frames, and whether it began.
            let mut in_hand: Option<(u64, u64, bool)> = None;
            let mut listens = 0_u8;
            let mut closed = Vec::new();
            let register =
                |recorder: &mut Recorder, span: u64, listens: &mut u8, draws: &mut Draws| {
                    // A seek carries on the open listen; anything else opens one.
                    let continues = recorder
                        .open()
                        .map(|open| open.started.listen_id)
                        .filter(|_| draws.below(3) == 0);
                    *listens += 1;
                    recorder.register(
                        span,
                        Span {
                            listen: started(*listens),
                            continues,
                            from_ms: i64::try_from(draws.below(100_000)).unwrap(),
                            rate: 1000,
                            end: None,
                        },
                    );
                };
            for _ in 0..60 {
                clock += draws.below(5_000);
                match draws.below(6) {
                    0 => {
                        // Something is put in place of the span in hand,
                        // registered before the output says so, as a seek's
                        // span is: one that carries the listen on keeps it.
                        recorder.note(EndReason::Next);
                        register(&mut recorder, next, &mut listens, &mut draws);
                        if let Some((span, frames, _)) = in_hand.take() {
                            say(
                                &mut recorder,
                                Event::Ended {
                                    span,
                                    frames,
                                    at: clock,
                                    end: End::Replaced,
                                },
                            );
                        }
                        in_hand = Some((next, 0, false));
                        next += 1;
                    }
                    1 | 2 => {
                        // Some of it is heard.
                        if let Some((span, frames, began)) = &mut in_hand {
                            let more = draws.below(3_000);
                            if more > 0 && !*began {
                                *began = true;
                                say(
                                    &mut recorder,
                                    Event::Began {
                                        span: *span,
                                        at: clock,
                                    },
                                );
                            }
                            *frames += more;
                            heard += more;
                        }
                    }
                    3 => {
                        // It ends into one queued after it.
                        if let Some((span, frames, _)) = in_hand.take() {
                            register(&mut recorder, next, &mut listens, &mut draws);
                            let end = End::Joined { next };
                            say(
                                &mut recorder,
                                Event::Ended {
                                    span,
                                    frames,
                                    at: clock,
                                    end,
                                },
                            );
                            in_hand = Some((next, 0, false));
                            next += 1;
                        }
                    }
                    4 => {
                        // Its audio runs out, or fails.
                        if let Some((span, frames, _)) = in_hand.take() {
                            let end = if draws.below(2) == 0 {
                                End::Drained
                            } else {
                                End::Failed("x".into())
                            };
                            say(
                                &mut recorder,
                                Event::Ended {
                                    span,
                                    frames,
                                    at: clock,
                                    end,
                                },
                            );
                        }
                    }
                    _ => {
                        // One queued and let go before it was reached.
                        register(&mut recorder, next, &mut listens, &mut draws);
                        say(
                            &mut recorder,
                            Event::Ended {
                                span: next,
                                frames: 0,
                                at: clock,
                                end: End::Replaced,
                            },
                        );
                        next += 1;
                    }
                }
                // A save now and then: what it writes closed is gone after.
                if draws.below(4) == 0 {
                    closed.extend(
                        recorder
                            .unsaved(0)
                            .into_iter()
                            .filter(|saved| saved.ended.is_some()),
                    );
                    recorder.saved(0);
                }
            }
            if let Some((span, frames, _)) = in_hand.take() {
                recorder.note(EndReason::Stopped);
                say(
                    &mut recorder,
                    Event::Ended {
                        span,
                        frames,
                        at: clock + 1,
                        end: End::Replaced,
                    },
                );
            }
            closed.extend(recorder.unsaved(0));
            assert!(
                recorder.open().is_none(),
                "seed {seed}: a listen was left open"
            );
            assert!(recorder.spans.is_empty(), "seed {seed}: a span never ended");
            let mut ids: Vec<Id> = closed.iter().map(|saved| saved.started.listen_id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(
                ids.len(),
                closed.len(),
                "seed {seed}: a listen closed twice"
            );
            // At a rate of 1000 a frame is a millisecond.
            let kept: i64 = closed.iter().map(|saved| saved.heard.ms_played).sum();
            assert_eq!(
                kept,
                i64::try_from(heard).unwrap(),
                "seed {seed}: frames heard and kept differ"
            );
            for saved in &closed {
                let ended = saved.ended.as_ref().unwrap();
                assert!(
                    saved.started.started_at_ms <= ended.ended_at_ms,
                    "seed {seed}"
                );
                assert!(
                    saved.heard.ms_played > 0,
                    "seed {seed}: a listen of nothing"
                );
                sought += usize::from(saved.heard.seek_count > 0);
            }
        }
        // Listens were carried on across seeks, not only opened and closed.
        assert!(sought > 100, "{sought} listens carried across a seek");
    }
}
