// `balanced` in this file is adapted from WebKit: InlineContentBalancer
// (Source/WebCore/layout/formattingContexts/inline/InlineContentBalancer.cpp,
// since renamed InlineContentConstrainer.cpp), its
// balanceRangeWithLineRequirement and its cost, ported to Rust. That part
// stays under WebKit's licence, which follows; the rest is under this
// project's (LICENSE).
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

//! Lean back: the whole window for what is playing, over a blur of its
//! cover, and one thing about it at a time ("History tab and Lean back" in
//! docs/internals/desktop.md). The facts are the server's (`history.track`),
//! and turn every fourteen seconds, or by ← and →.
//!
//! It opens from the player bar's button or View › Lean Back (⇧⌘F), and
//! closes with Esc, ⇧⌘F, Back or its own button; the shell holds it while
//! it is open. The cover travels from the player bar on the way in and back
//! on the way out, as a cover does between pages, while the view fades in or
//! out under it. It follows the player as it goes.
//!
//! Layout sizes are computed from the window dimensions (`Sizes`), and each
//! part is positioned explicitly. The words are set with letter-spacing
//! tracking (`tracked.rs`) and balanced line breaks, so that the widest fact
//! is the width the group is centred on.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Bounds, BoxShadow, Context, Entity, EventEmitter, FontWeight, Hsla,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, Pixels, Render, RenderImage,
    SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription, Task, Window,
    WindowControlArea, div, img, linear_color_stop, linear_gradient, prelude::FluentBuilder, px,
};
use ritornel_contract::methods::{HistoryTrack, QueueWindow as ReadQueue};
use ritornel_contract::{
    Event, Fact, FactKind, Id, Phrase, QueueEntry, QueueOrder, QueueWindowParams, SkipDirection,
    TrackHistoryParams, TrackSummary,
};

use crate::bar::{clock, tabular};
use crate::icons::icon;
use crate::link::{Link, LinkEvent};
use crate::moves::{arrive_curve, ease, ease_in, ease_out, fly_curve, progress, since};
use crate::net::Net;
use crate::player::{Player, stars_of};
use crate::theme;
use crate::tracked::{tracked, tracked_runs};

/// How long each fact stays before the next.
pub(crate) const FACT_MS: u64 = 14_000;
/// A head longer than this is set a size smaller.
const LONG_HEAD: usize = 60;
/// The view comes up slowly and goes in the time the cover takes to return.
const OPEN_MS: f32 = 500.;
const CLOSE_MS: f32 = crate::moves::FLY_MS;
/// A fact gives way before the next comes up.
const FADE_MS: f32 = 240.;
/// The largest the cover is drawn, in points: at 2,000 points across, 30% of the window.
const COVER: f32 = 600.;
/// How long the group takes to find its new middle when a new record's
/// facts are wider or narrower.
const GLIDE_MS: f32 = 400.;
/// A fact comes up (`@keyframes rise`, 0.7 s), the line under it 80 ms behind.
const RISE_MS: f32 = 700.;
const RISE_DELAY_MS: f32 = 80.;
/// A new record's cover comes up over the last (`comeUp(…, 400)`).
const FACE_MS: f32 = 400.;
/// The dot of the fact shown changes colour (`transition: background-color 0.3s`).
const DOT_MS: f32 = 300.;
/// The blur behind drifts one way and back (`animation: drift 44s ease-in-out infinite alternate`).
const DRIFT_MS: f32 = 44_000.;
/// How long the view waits, unseen, for the large cover, and for the words
/// to be measured, so that what travels is sharp and lands where it stays.
const WAIT_COVER: Duration = Duration::from_millis(250);
const WAIT_FIT: Duration = Duration::from_millis(400);
/// How long a new record's pictures are waited for, so that they come up whole.
const WAIT_PICTURE: Duration = Duration::from_millis(1500);
/// The height of the bar along the foot: the play button, 46 points, and
/// 22 under it; and the hairline under that.
const FOOT: f32 = 46.;
const FOOT_PAD: f32 = 22.;
const HAIRLINE: f32 = 3.;
/// The row of the window's own buttons, over the top.
const TOP: f32 = 52.;
/// The line over the facts: 14 points on the body's line of 1.4.
const KICK: f32 = 14.;

/// A line's height as WebKit lays it out: its `line-height` in whole
/// points, the rest cut off, so that 1.4 of 14 points is 19, not 19.6.
fn line(height: f32) -> f32 {
    height.floor()
}

/// What a song with no history shows: its title in large type, and its artist.
pub(crate) fn plain_fact(track: &TrackSummary) -> Fact {
    Fact {
        kind: FactKind::SongPlays,
        head: vec![Phrase {
            text: track.title.clone().unwrap_or_else(|| "Untitled".to_owned()),
            strong: true,
        }],
        more: Some(
            track
                .artist
                .clone()
                .unwrap_or_else(|| "Unknown artist".to_owned()),
        ),
    }
}

/// The facts to rotate through: the server's, or the song's title and
/// artist alone when it has none.
pub(crate) fn facts_for(track: &TrackSummary, facts: Option<&[Fact]>) -> Vec<Fact> {
    match facts {
        Some(facts) if !facts.is_empty() => facts.to_vec(),
        _ => vec![plain_fact(track)],
    }
}

/// Whether a fact's head is long enough to be set a size smaller, counted
/// as JavaScript counts a string's length.
pub(crate) fn long(fact: &Fact) -> bool {
    fact.head
        .iter()
        .map(|phrase| phrase.text.encode_utf16().count())
        .sum::<usize>()
        > LONG_HEAD
}

/// The line over the facts: the record and its artist, or "Now playing"
/// for a song on none.
pub(crate) fn kicker(track: &TrackSummary) -> String {
    match (&track.album, &track.artist) {
        (None, _) => "Now playing".to_owned(),
        (Some(album), None) => album.clone(),
        (Some(album), Some(artist)) => format!("{album} · {artist}"),
    }
}

/// The fact `by` places on from `at`, going round.
pub(crate) fn stepped(at: usize, by: i64, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let count = count as i64;
    ((at as i64 + by).rem_euclid(count)) as usize
}

/// CSS's `clamp(min, value, max)`: the minimum wins over the maximum.
fn clamp(min: f32, value: f32, max: f32) -> f32 {
    min.max(value.min(max))
}

/// The view's sizes for a window: up to about 1,680 points across they use
/// the base dimensions; past that the cover, the type and its measure grow
/// with the window; a short window gets smaller type and a smaller cover,
/// not an overlap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Sizes {
    /// The least margin at each side (`--side`).
    pub side: f32,
    /// The size of a fact (`--h`).
    pub h: f32,
    /// The cover's side (`--cover`).
    pub cover: f32,
    /// Between the cover and the words (`--gap`).
    pub gap: f32,
    /// The widest the words are set (`--measure`).
    pub measure: f32,
    /// What is left for them beside the cover (`--avail`).
    pub avail: f32,
    /// Under a fact's head.
    pub below: f32,
    /// The size of the line under it.
    pub more: f32,
}

impl Sizes {
    pub(crate) fn of(width: f32, height: f32) -> Self {
        let (vw, vh) = (width / 100., height / 100.);
        let side = clamp(40., 5.6 * vw, 72.);
        let h = clamp(32., (8.4 * vh).min(60f32.max(3.6 * vw)), 76.);
        let cover = clamp(240., (56. * vh).min(32. * vw), 440f32.max(30. * vw));
        let gap = clamp(48., 6. * vw, 80.);
        Self {
            side,
            h,
            cover,
            gap,
            measure: h * 12.5,
            avail: width - 2. * side - cover - gap,
            below: clamp(10., 2.8 * vh, 20.),
            more: clamp(15., h * 0.32, 23.),
        }
    }

    /// The words' width (`--text`): the widest of the facts as set, within
    /// the measure and the room there is.
    pub(crate) fn text(&self, fit: Option<f32>) -> f32 {
        fit.unwrap_or(self.measure)
            .min(self.measure)
            .min(self.avail)
    }

    /// The margin at each side (`--edge`): what is left over, shared.
    pub(crate) fn edge(&self, width: f32, text: f32) -> f32 {
        (width - self.cover - self.gap - text) / 2.
    }

    /// Where the ruler sets the facts to be measured: as wide as they may be.
    fn ruler(&self) -> f32 {
        self.measure.min(self.avail)
    }
}

/// Lines of words, as `text-wrap` would break them at `width`: as many
/// words to a line as fit, a word wider than the line on one of its own.
/// `measure` is the width of a line of the words in a range.
fn greedy(count: usize, width: f32, measure: &dyn Fn(Range<usize>) -> f32) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < count {
        let mut end = start + 1;
        while end < count && measure(start..end + 1) <= width {
            end += 1;
        }
        lines.push(start..end);
        start = end;
    }
    lines
}

/// Lines of words as WebKit's `text-wrap: balance` breaks them
/// (`InlineContentBalancer`): as many lines as they would take anyway, each
/// as near as it can be to an equal share of the whole, by the least sum of
/// the squares of how far each falls short of it or passes it.
pub(crate) fn balanced(
    count: usize,
    width: f32,
    measure: &dyn Fn(Range<usize>) -> f32,
) -> Vec<Range<usize>> {
    let plain = greedy(count, width, measure);
    let lines = plain.len();
    if lines < 2 {
        return plain;
    }
    let ideal = measure(0..count) / lines as f32;
    // best[k][i]: the least cost of the first i words in k lines, and where
    // the last of those lines starts.
    let mut best = vec![vec![(f32::INFINITY, 0usize); count + 1]; lines + 1];
    if let Some(start) = best.get_mut(0).and_then(|row| row.get_mut(0)) {
        *start = (0., 0);
    }
    for k in 1..=lines {
        for i in k..=count {
            let mut found = (f32::INFINITY, 0usize);
            for j in (k - 1)..i {
                let before = best
                    .get(k - 1)
                    .and_then(|row| row.get(j))
                    .map_or(f32::INFINITY, |cell| cell.0);
                if !before.is_finite() {
                    continue;
                }
                let line = measure(j..i);
                if line > width && i - j > 1 {
                    continue;
                }
                let cost = before + (ideal - line).powi(2);
                if cost < found.0 {
                    found = (cost, j);
                }
            }
            if let Some(cell) = best.get_mut(k).and_then(|row| row.get_mut(i)) {
                *cell = found;
            }
        }
    }
    let mut ranges = Vec::with_capacity(lines);
    let mut end = count;
    for k in (1..=lines).rev() {
        let Some(&(cost, start)) = best.get(k).and_then(|row| row.get(end)) else {
            return plain;
        };
        if !cost.is_finite() {
            return plain;
        }
        ranges.push(start..end);
        end = start;
    }
    ranges.reverse();
    ranges
}

/// The words of `text`, as byte ranges: what lies between its spaces.
fn words(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut start = None;
    for (at, character) in text.char_indices() {
        match (character == ' ', start) {
            (true, Some(from)) => {
                found.push(from..at);
                start = None;
            }
            (false, None) => start = Some(at),
            _ => {}
        }
    }
    if let Some(from) = start {
        found.push(from..text.len());
    }
    found
}

/// The bytes of `text` that words `range` of `words` take, spaces between included.
fn span(words: &[Range<usize>], range: &Range<usize>) -> Range<usize> {
    let start = words.get(range.start).map_or(0, |word| word.start);
    let end = words
        .get(range.end.saturating_sub(1))
        .map_or(start, |word| word.end);
    start..end
}

/// A line of a fact's head: its bytes, and how many of them are each set
/// apart or not, in order.
type HeadLine = (Range<usize>, Vec<(usize, bool)>);

/// A cover's shadow at an opacity, scaled by so much.
type Shadow = fn(f32, f32) -> BoxShadow;

/// How a fact is set: its head's size and tracking, and the lines it and
/// the line under it break into.
#[derive(Debug, Clone)]
struct Set {
    size: f32,
    tracking: f32,
    head: String,
    /// Each line of the head: its bytes, and their colours.
    lines: Vec<HeadLine>,
    more: Vec<String>,
    /// How far its type reaches across, at most.
    reach: f32,
}

/// One picture of the record playing, as the cover and as the ground behind everything.
struct Picture {
    key: String,
    /// Instant gradient fallback derived from its name while the picture loads.
    soft: (Hsla, Hsla),
    /// Soft glow artwork generated off-thread only when the record has no cover.
    fill: Option<Arc<RenderImage>>,
    /// The cover, large, once it is read.
    large: Option<Arc<RenderImage>>,
    /// The cover blurred, and whether for the dark palette.
    ground: Option<(Arc<RenderImage>, bool)>,
    /// When it came up over the last; the first is simply there.
    since: Option<Instant>,
}

/// What the server said of a track, or `None` for nothing, which is said plainly.
struct Read {
    appearance: Id,
    facts: Option<Vec<Fact>>,
    revision: u64,
}

impl Read {
    fn for_track(&self, track: &TrackSummary) -> Option<Vec<Fact>> {
        (self.appearance == track.appearance_id).then(|| facts_for(track, self.facts.as_deref()))
    }
}

/// How the view is coming or going.
#[derive(Debug, Clone, Copy)]
enum Stage {
    /// Held back, unseen, until the large cover is in hand and the words are measured.
    Waiting {
        asked: Instant,
    },
    /// Fading in, the cover travelling from the bar's.
    Arriving {
        start: Instant,
        from: Bounds<Pixels>,
    },
    Open,
    /// Fading out, the cover travelling back to the bar's.
    Leaving {
        start: Instant,
        from: Bounds<Pixels>,
    },
}

/// What Lean back tells the window.
pub(crate) enum LeanEvent {
    /// The player bar's cover is to be hidden while a copy of it travels, or shown again.
    BarCover(bool),
    /// It has gone.
    Gone,
}

impl EventEmitter<LeanEvent> for LeanBack {}

pub(crate) struct LeanBack {
    player: Entity<Player>,
    net: Option<Arc<Net>>,
    stage: Stage,
    /// Accumulated playback drift time, and when the current drift span
    /// started, so pausing freezes the drift without jumping on resume.
    drift_elapsed: Duration,
    drift_since: Option<Instant>,
    pictures: Vec<Picture>,
    /// The picture being read, what reads it, and what shows it if it is late.
    reading: Option<(String, Task<()>, Task<()>)>,
    /// A record's ground being read again for the other palette.
    retoning: Option<Task<()>>,
    /// The record the facts are of, and the song.
    record: Option<Id>,
    appearance: Option<Id>,
    read: Option<Read>,
    /// Bumped whenever what is read changes, for the measurements kept.
    read_count: u64,
    asking: Option<Task<()>>,
    /// Which fact is shown, and since when (for its rise).
    at: usize,
    fact_key: String,
    fact_since: Instant,
    /// A fact giving way, since when, and the one that comes next.
    stepping: Option<(Instant, usize)>,
    timer: Task<()>,
    /// The dot lit before, and when the one lit now was.
    dot: (usize, Option<Instant>),
    /// The words' width, as measured, what for, and the last measurement.
    fit: Option<f32>,
    fit_for: String,
    measured: Option<(String, f32)>,
    /// The edge the group glides from, and since when.
    glide: Option<(f32, Instant)>,
    /// The edge as last drawn.
    shown_edge: Option<f32>,
    /// How each fact was set, by what it says and how wide it was set.
    sets: HashMap<(String, u32, u32), Set>,
    /// What plays next.
    next: Option<QueueEntry>,
    watched: (Option<Id>, u64, bool),
    reading_next: Option<Task<()>>,
    star: Option<u32>,
    /// Where the cover was last drawn, for its way back.
    cover_at: Option<Bounds<Pixels>>,
    /// When the drift was last drawn, while nothing else moves.
    drift_tick: Option<Task<()>>,
    _link: Subscription,
    _player: Subscription,
}

impl LeanBack {
    pub(crate) fn new(link: Entity<Link>, player: Entity<Player>, cx: &mut Context<Self>) -> Self {
        let heard = cx.subscribe(
            &link,
            |lean: &mut Self, _, event: &LinkEvent, cx| match event {
                LinkEvent::Connected(net) => {
                    lean.net = Some(Arc::clone(net));
                    if let Some(appearance) = lean.appearance {
                        lean.read_facts(appearance, cx);
                    }
                    lean.read_next(cx);
                }
                LinkEvent::Away => lean.net = None,
                // The same session: nothing it read has changed.
                LinkEvent::Restated(net) => lean.net = Some(Arc::clone(net)),
                LinkEvent::Server(Event::Library(event)) => {
                    if let Some(appearance) = lean.appearance
                        && lean
                            .read
                            .as_ref()
                            .is_none_or(|read| event.library_revision > read.revision)
                    {
                        lean.read_facts(appearance, cx);
                    }
                }
                LinkEvent::Server(Event::Queue(_)) => lean.read_next(cx),
                LinkEvent::Server(_) => {}
            },
        );
        let followed = cx.observe(&player, |lean: &mut Self, _, cx| lean.follow(cx));
        let net = link.read(cx).net();
        let now = Instant::now();
        let stage = if crate::platform::reduce_motion() {
            Stage::Open
        } else {
            Stage::Waiting { asked: now }
        };
        let mut lean = Self {
            player,
            net,
            stage,
            drift_elapsed: Duration::ZERO,
            drift_since: None,
            pictures: Vec::new(),
            reading: None,
            retoning: None,
            record: None,
            appearance: None,
            read: None,
            read_count: 0,
            asking: None,
            at: 0,
            fact_key: String::new(),
            fact_since: now,
            stepping: None,
            timer: Task::ready(()),
            dot: (0, None),
            fit: None,
            fit_for: String::new(),
            measured: None,
            glide: None,
            shown_edge: None,
            sets: HashMap::new(),
            next: None,
            watched: (None, u64::MAX, false),
            reading_next: None,
            star: None,
            cover_at: None,
            drift_tick: None,
            _link: heard,
            _player: followed,
        };
        cx.on_release(|lean, cx| {
            lean.release_images(cx);
        })
        .detach();
        lean.turn_every(cx);
        lean.follow(cx);
        crate::debug(format_args!("lean: opened"));
        lean
    }

    /// Drops the large covers, soft placeholders, and blurred grounds this view
    /// owns from the sprite atlas.
    fn release_images(&mut self, cx: &mut gpui::App) {
        let mut released = false;
        for picture in self.pictures.drain(..) {
            if let Some(fill) = picture.fill {
                released = true;
                crate::covers::drop_unshared(fill, None, cx);
            }
            if let Some(large) = picture.large {
                released = true;
                crate::covers::drop_unshared(large, None, cx);
            }
            if let Some((ground, _)) = picture.ground {
                released = true;
                crate::covers::drop_unshared(ground, None, cx);
            }
        }
        if released {
            crate::platform::give_back();
        }
    }

    /// Whether it is on its way out.
    pub(crate) fn leaving(&self) -> bool {
        matches!(self.stage, Stage::Leaving { .. })
    }

    /// Whether it covers the window whole, so that nothing under it need be drawn.
    pub(crate) fn covers(&self) -> bool {
        matches!(self.stage, Stage::Open)
    }

    /// A new song gets its own facts. A new record starts at the first one.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let player = self.player.read(cx);
        let Some(state) = player.state.clone() else {
            self.close(cx);
            return;
        };
        if state.entry_id.is_none() {
            self.close(cx);
            return;
        }
        let watched = (state.entry_id, state.queue_revision, state.shuffle);
        if watched != self.watched {
            self.watched = watched;
            self.read_next(cx);
        }
        if let Some(track) = &state.track
            && self.appearance != Some(track.appearance_id)
        {
            self.appearance = Some(track.appearance_id);
            self.read = None;
            let of = track.release_id.unwrap_or(track.appearance_id);
            if self.record != Some(of) {
                self.record = Some(of);
                self.read_count += 1;
                self.at = 0;
                self.stepping = None;
            }
            self.read_facts(track.appearance_id, cx);
        }
        cx.notify();
    }

    fn read_facts(&mut self, appearance: Id, cx: &mut Context<Self>) {
        let Some(net) = self.net.clone() else { return };
        let client = Arc::clone(&net.client);
        let call = net.rt().spawn(async move {
            client
                .call::<HistoryTrack>(&TrackHistoryParams {
                    appearance_id: appearance,
                })
                .await
        });
        self.asking = Some(cx.spawn(async move |this, cx| {
            let answer = call.await.ok().and_then(Result::ok);
            this.update(cx, |lean, cx| {
                if lean.appearance != Some(appearance) {
                    return;
                }
                lean.read = Some(match answer {
                    Some(history) => Read {
                        appearance,
                        facts: Some(history.facts),
                        revision: history.library_revision,
                    },
                    None => Read {
                        appearance,
                        facts: None,
                        revision: 0,
                    },
                });
                lean.read_count += 1;
                cx.notify();
            })
            .ok();
        }));
    }

    fn read_next(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.net.clone() else { return };
        let client = Arc::clone(&net.client);
        // Lean back follows the place played on.
        let target = self.player.read(cx).target();
        let call = net.rt().spawn(async move {
            client
                .call::<ReadQueue>(&QueueWindowParams {
                    player: target,
                    offset: 0,
                    limit: Some(1),
                    order: Some(QueueOrder::UpNext),
                })
                .await
        });
        self.reading_next = Some(cx.spawn(async move |this, cx| {
            // The last read stands until one works.
            if let Some(window) = call.await.ok().and_then(Result::ok) {
                this.update(cx, |lean, cx| {
                    lean.next = window.entries.into_iter().next();
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// The facts shown now, once read.
    fn facts(&self, track: &TrackSummary) -> Option<Vec<Fact>> {
        self.read.as_ref().and_then(|read| read.for_track(track))
    }

    fn fact_count(&self, cx: &gpui::App) -> usize {
        self.player
            .read(cx)
            .state
            .as_ref()
            .and_then(|state| state.track.as_ref())
            .and_then(|track| self.facts(track))
            .map_or(0, |facts| facts.len())
    }

    /// Turns to the fact `by` places on: the one shown gives way, and the next comes up.
    fn step(&mut self, by: i64, cx: &mut Context<Self>) {
        let count = self.fact_count(cx);
        if count < 2 || self.stepping.is_some() {
            return;
        }
        let to = stepped(self.at % count, by, count);
        if crate::platform::reduce_motion() {
            self.at = to;
        } else {
            self.stepping = Some((Instant::now(), to));
        }
        cx.notify();
    }

    fn turn_every(&mut self, cx: &mut Context<Self>) {
        self.timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(FACT_MS))
                    .await;
                if this.update(cx, |lean, cx| lean.step(1, cx)).is_err() {
                    break;
                }
            }
        });
    }

    /// ← or →: the fact before or after, and the turning starts again from
    /// now. Not on its way out.
    pub(crate) fn key_step(&mut self, by: i64, cx: &mut Context<Self>) {
        if self.leaving() {
            return;
        }
        self.step(by, cx);
        self.turn_every(cx);
    }

    /// Starts it on its way out. It is gone once its view says so.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if self.leaving() {
            return;
        }
        if crate::platform::reduce_motion() {
            crate::debug(format_args!("lean: closed"));
            cx.emit(LeanEvent::Gone);
            return;
        }
        let from = self.cover_at.unwrap_or_default();
        self.stage = Stage::Leaving {
            start: Instant::now(),
            from,
        };
        cx.emit(LeanEvent::BarCover(true));
        cx.notify();
    }

    /// Reads the picture of the record playing: the large cover and its
    /// blur. The first is simply there, filled in as it is read; a new
    /// record's comes up whole over the last, once read or after a while.
    fn want_picture(
        &mut self,
        track: &TrackSummary,
        sizes: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = match track.art_id {
            Some(art) => art.to_string(),
            None => format!(
                "{}:{}",
                track
                    .album
                    .as_deref()
                    .or(track.title.as_deref())
                    .unwrap_or(""),
                track.artist.as_deref().unwrap_or("")
            ),
        };
        let dark = theme::dark();
        let (width, height) = sizes;
        let radius = crate::blur::radius(crate::blur::WASH.0, width.max(height) + 180.);
        let (saturation, brightness) = crate::blur::wash_tone(dark);
        let name = format!(
            "{}{}",
            track
                .album
                .as_deref()
                .or(track.title.as_deref())
                .unwrap_or(""),
            track.artist.as_deref().unwrap_or("")
        );
        // The ground, toned for this palette, for the record shown.
        if let Some(top) = self.pictures.last()
            && top.key == key
        {
            if top.ground.as_ref().is_some_and(|(_, toned)| *toned != dark)
                && self.retoning.is_none()
            {
                let read = self.read_ground(track.art_id, &name, radius, saturation, brightness);
                let key = key.clone();
                self.retoning = Some(cx.spawn(async move |this, cx| {
                    let ground = read.await;
                    this.update(cx, |lean, cx| {
                        lean.retoning = None;
                        if let (Some(ground), Some(top)) = (ground, lean.pictures.last_mut())
                            && top.key == key
                            && let Some((old, _)) = top.ground.replace((Arc::new(ground), dark))
                        {
                            crate::covers::drop_unshared(old, None, cx);
                        }
                        cx.notify();
                    })
                    .ok();
                }));
            }
            return;
        }
        if self.reading.as_ref().is_some_and(|(at, _, _)| *at == key) {
            return;
        }
        let soft = theme::soft(&name);
        let first = self.pictures.is_empty() || crate::platform::reduce_motion();
        if first {
            for old in self.pictures.drain(..) {
                if let Some(fill) = old.fill {
                    crate::covers::drop_unshared(fill, Some(window), cx);
                }
                if let Some(large) = old.large {
                    crate::covers::drop_unshared(large, Some(window), cx);
                }
                if let Some((ground, _)) = old.ground {
                    crate::covers::drop_unshared(ground, Some(window), cx);
                }
            }
            self.pictures.push(Picture {
                key: key.clone(),
                soft,
                fill: None,
                large: None,
                ground: None,
                since: None,
            });
        }
        let large = self.read_large(track.art_id, &name, window);
        let ground = self.read_ground(track.art_id, &name, radius, saturation, brightness);
        let read_key = key.clone();
        let read = cx.spawn(async move |this, cx| {
            let ((large, fill), ground) = (large.await, ground.await);
            this.update(cx, |lean, cx| {
                lean.took_picture(&read_key, soft, fill, large, ground, dark, cx);
                cx.notify();
            })
            .ok();
        });
        // Late, a new record's picture comes up as it is, and is filled in once read.
        let late_key = key.clone();
        let late = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WAIT_PICTURE).await;
            this.update(cx, |lean, cx| {
                lean.took_picture(&late_key, soft, None, None, None, dark, cx);
                cx.notify();
            })
            .ok();
        });
        self.reading = Some((key, read, late));
    }

    /// Takes a record's pictures in: into its place if it is shown, or as a
    /// new picture over the last.
    #[allow(clippy::too_many_arguments)]
    fn took_picture(
        &mut self,
        key: &str,
        soft: (Hsla, Hsla),
        fill: Option<RenderImage>,
        large: Option<RenderImage>,
        ground: Option<RenderImage>,
        dark: bool,
        cx: &mut gpui::App,
    ) {
        if let Some(shown) = self.pictures.iter_mut().find(|each| each.key == key) {
            if let Some(fill) = fill
                && let Some(old) = shown.fill.replace(Arc::new(fill))
            {
                crate::covers::drop_unshared(old, None, cx);
            }
            if let Some(large) = large
                && let Some(old) = shown.large.replace(Arc::new(large))
            {
                crate::covers::drop_unshared(old, None, cx);
            }
            if let Some(ground) = ground
                && let Some((old, _)) = shown.ground.replace((Arc::new(ground), dark))
            {
                crate::covers::drop_unshared(old, None, cx);
            }
            return;
        }
        self.pictures.push(Picture {
            key: key.to_owned(),
            soft,
            fill: fill.map(Arc::new),
            large: large.map(Arc::new),
            ground: ground.map(|ground| (Arc::new(ground), dark)),
            since: Some(Instant::now()),
        });
    }

    fn read_large(
        &self,
        art: Option<Id>,
        name: &str,
        window: &Window,
    ) -> impl std::future::Future<Output = (Option<RenderImage>, Option<RenderImage>)> + 'static
    {
        let px = (COVER * window.scale_factor()).ceil() as u32;
        let name = name.to_owned();
        let read = self.net.clone().map(|net| {
            let sizes = net.art_sizes.clone();
            let http = Arc::clone(&net.art);
            crate::covers::AbortOnDrop::new(net.rt().spawn(async move {
                if let Some(art) = art
                    && let Ok((image, _, _)) =
                        crate::covers::fetch_square(&http, &sizes, art, px).await
                {
                    return (Some(image), None);
                }
                let fill = crate::covers::decode(move || Some(theme::soft_image(&name, px))).await;
                (None, fill)
            }))
        });
        async move {
            match read {
                Some(read) => read.await.unwrap_or((None, None)),
                None => (None, None),
            }
        }
    }

    /// The cover blurred, or for a record with none its soft colour blurred
    /// for the background.
    fn read_ground(
        &self,
        art: Option<Id>,
        name: &str,
        radius: usize,
        saturation: f32,
        brightness: f32,
    ) -> impl std::future::Future<Output = Option<RenderImage>> + 'static {
        let soft = art
            .is_none()
            .then(|| theme::soft_pixels(name, crate::blur::SIDE));
        let net = self.net.clone();
        let read = match (art, net) {
            (Some(art), Some(net)) => {
                let http = Arc::clone(&net.art);
                Some(crate::covers::AbortOnDrop::new(net.rt().spawn(
                    async move {
                        let bytes = http.get(art, crate::blur::SIDE).await.ok()?;
                        crate::covers::decode(move || {
                            crate::blur::jpeg_toned(&bytes, radius, saturation, brightness)
                        })
                        .await
                    },
                )))
            }
            (None, Some(net)) => soft.map(|soft| {
                crate::covers::AbortOnDrop::new(net.rt().spawn(async move {
                    crate::covers::decode(move || {
                        Some(crate::blur::toned(&soft, radius, saturation, brightness))
                    })
                    .await
                }))
            }),
            _ => None,
        };
        async move {
            match read {
                Some(read) => read.await.ok().flatten(),
                None => None,
            }
        }
    }

    /// How `fact` is set at `width`, kept.
    fn set(&mut self, fact: &Fact, sizes: &Sizes, width: f32, window: &Window) -> Set {
        let head: String = fact
            .head
            .iter()
            .map(|phrase| phrase.text.as_str())
            .collect();
        let key = (
            format!("{head}\u{0}{}", fact.more.as_deref().unwrap_or("")),
            width.to_bits(),
            sizes.h.to_bits(),
        );
        if let Some(set) = self.sets.get(&key) {
            return set.clone();
        }
        let small = long(fact);
        let (size, tracking) = if small {
            (sizes.h * 0.8, -0.03)
        } else {
            (sizes.h, -0.035)
        };
        let weight = FontWeight(700.);
        let ink = theme::ink();
        let found = words(&head);
        let measure_head = |range: Range<usize>| {
            let bytes = span(&found, &range);
            head.get(bytes).map_or(0., |text| {
                tracked(text.to_owned(), size, size, weight, ink, tracking).width(window)
            })
        };
        let breaks = balanced(found.len(), width, &measure_head);
        // Each phrase's bytes, and whether it is set apart.
        let mut phrases = Vec::new();
        let mut at = 0;
        for phrase in &fact.head {
            phrases.push((at..at + phrase.text.len(), phrase.strong));
            at += phrase.text.len();
        }
        let mut lines = Vec::new();
        let mut reach = 0f32;
        for line in &breaks {
            let bytes = span(&found, line);
            // WebKit's letter-spacing follows the last letter too, and
            // the ruler measures it with the rest.
            reach = reach.max(measure_head(line.clone()) + tracking * size);
            let runs: Vec<(usize, bool)> = phrases
                .iter()
                .filter_map(|(range, strong)| {
                    let start = range.start.max(bytes.start);
                    let end = range.end.min(bytes.end);
                    (end > start).then_some((end - start, *strong))
                })
                .collect();
            lines.push((bytes, runs));
        }
        let mut more = Vec::new();
        if let Some(text) = &fact.more {
            let found = words(text);
            let measure_more = |range: Range<usize>| {
                text.get(span(&found, &range)).map_or(0., |line| {
                    tracked(
                        line.to_owned(),
                        sizes.more,
                        sizes.more,
                        FontWeight::NORMAL,
                        ink,
                        0.,
                    )
                    .width(window)
                })
            };
            let wrap = width.min(sizes.h * 10.3);
            for line in greedy(found.len(), wrap, &measure_more) {
                reach = reach.max(measure_more(line.clone()));
                if let Some(words) = text.get(span(&found, &line)) {
                    more.push(words.to_owned());
                }
            }
        }
        let set = Set {
            size,
            tracking,
            head,
            lines,
            more,
            reach,
        };
        if self.sets.len() > 64 {
            self.sets.clear();
        }
        self.sets.insert(key, set.clone());
        set
    }

    /// Measures the words as the ruler does: the widest of the facts it
    /// turns through, and the line over them, so that the group hugs them
    /// and stays put as they turn. Within a record the width only grows; a
    /// new record may be narrower, and the group then glides to its new
    /// middle.
    fn refit(
        &mut self,
        track: &TrackSummary,
        facts: &[Fact],
        sizes: &Sizes,
        size: (f32, f32),
        window: &Window,
    ) {
        let ruler = sizes.ruler();
        let measuring = format!(
            "{}:{}:{}x{}",
            self.read_count, track.appearance_id, size.0, size.1
        );
        let measured = match &self.measured {
            Some((key, measured)) if *key == measuring => *measured,
            _ => {
                let kick = kicker(track);
                let mut reach = tracked(
                    kick,
                    KICK,
                    line(KICK * 1.4),
                    FontWeight(600.),
                    theme::ink(),
                    0.01,
                )
                .width(window)
                    + 0.01 * KICK;
                for fact in facts {
                    reach = reach.max(self.set(fact, sizes, ruler, window).reach);
                }
                let measured = reach.ceil() + 2.;
                self.measured = Some((measuring, measured));
                measured
            }
        };
        let size_key = format!("{}x{}", size.0, size.1);
        let key = format!(
            "{}:{size_key}",
            self.record
                .map(|record| record.to_string())
                .unwrap_or_default()
        );
        let resized = !self.fit_for.ends_with(&format!(":{size_key}"));
        let next = match self.fit {
            Some(fit) if key == self.fit_for => fit.max(measured),
            _ => measured,
        };
        self.fit_for = key;
        if Some(next) == self.fit {
            return;
        }
        let first = self.fit.is_none();
        let before = self.shown_edge;
        self.fit = Some(next);
        if first || resized || crate::platform::reduce_motion() {
            self.glide = None;
            return;
        }
        if let Some(before) = before {
            self.glide = Some((before, Instant::now()));
        }
    }
}

/// A cover drawn as a face: its soft colour, and the picture over it.
fn face(
    soft: (Hsla, Hsla),
    fill: Option<&Arc<RenderImage>>,
    large: Option<&Arc<RenderImage>>,
    bounds: Bounds<Pixels>,
    radius: f32,
    opacity: f32,
) -> AnyElement {
    let side = f32::from(bounds.size.width);
    div()
        .absolute()
        .left(bounds.origin.x)
        .top(bounds.origin.y)
        .size(px(side))
        .rounded(px(radius))
        .bg(linear_gradient(
            180.,
            linear_color_stop(soft.0, 0.),
            linear_color_stop(soft.1, 1.),
        ))
        .opacity(opacity)
        .when_some(fill, |face, fill| {
            face.child(
                img(Arc::clone(fill))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size(px(side))
                    .rounded(px(radius))
                    .object_fit(ObjectFit::Cover),
            )
        })
        .when_some(large, |face, large| {
            face.child(
                img(Arc::clone(large))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size(px(side))
                    .rounded(px(radius))
                    .object_fit(ObjectFit::Cover),
            )
        })
        .into_any_element()
}

/// The large cover's shadow, in each palette.
fn cover_shadow(opacity: f32, scale: f32) -> BoxShadow {
    let (y, blur, color) = if theme::dark() {
        (30., 80., gpui::rgba(0x00000073))
    } else {
        (18., 44., gpui::rgba(0x28261e38))
    };
    let color = gpui::Rgba {
        a: color.a * opacity,
        ..color
    };
    theme::css_shadow(y * scale, blur * scale, color)
}

/// The player bar cover's ring: `0 0 0 1px var(--line)`.
fn ring_shadow(opacity: f32, scale: f32) -> BoxShadow {
    let line = theme::line();
    let color = gpui::Rgba {
        a: line.a * opacity,
        ..line
    };
    BoxShadow::new(px(0.), px(0.), Hsla::from(color)).spread_radius(px(scale))
}

/// The copy of the cover that travels between the bar and the view
/// (`motion.ts`, `Flight`): from `from` to `to`, `t` of the way through its
/// fifth of a second. It is drawn as each end draws its cover, with its
/// corners and its shadow, moved and scaled from its top left corner, and
/// changes from the one to the other on the way by opacity alone: the new
/// face comes in over the old before the old goes, so the picture never
/// thins.
#[allow(clippy::too_many_arguments)]
fn flight(
    from: Bounds<Pixels>,
    to: Bounds<Pixels>,
    t: f32,
    radius: (f32, f32),
    shadows: (Shadow, Shadow),
    soft: (Hsla, Hsla),
    fill: Option<&Arc<RenderImage>>,
    large: Option<&Arc<RenderImage>>,
) -> AnyElement {
    let p = fly_curve(t);
    let lerp = |a: Pixels, b: Pixels| a + (b - a) * p;
    let bounds = Bounds::new(
        gpui::point(
            lerp(from.origin.x, to.origin.x),
            lerp(from.origin.y, to.origin.y),
        ),
        gpui::size(
            lerp(from.size.width, to.size.width),
            lerp(from.size.height, to.size.height),
        ),
    );
    let start = f32::from(from.size.width).max(1.);
    let end = f32::from(to.size.width).max(1.);
    // How far the copy is scaled from its start, and the scale it lands at.
    let scale = f32::from(bounds.size.width) / start;
    let landing = end / start;
    let old = if p < 0.5 { 1. } else { 1. - (p - 0.5) / 0.5 };
    let new = (p / 0.5).min(1.);
    let side = f32::from(bounds.size.width);
    let shadow = |make: Shadow, opacity: f32, by: f32, round: f32| {
        div()
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .size(px(side))
            .rounded(px(round))
            .shadow(vec![make(opacity, by)])
            .into_any_element()
    };
    let (old_round, new_round) = (radius.0 * scale, radius.1 / landing * scale);
    div()
        .absolute()
        .size_full()
        .child(shadow(shadows.0, 1. - p, scale, old_round))
        .child(shadow(shadows.1, p, scale / landing, new_round))
        .child(face(soft, fill, large, bounds, old_round, old))
        .child(face(soft, fill, large, bounds, new_round, new))
        .into_any_element()
}

impl Render for LeanBack {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let still = crate::platform::reduce_motion();
        let viewport = window.viewport_size();
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let sizes = Sizes::of(width, height);
        let player = self.player.read(cx);
        let state = player.state.clone();
        let playing = player.playing();
        let position = player.position();
        let own = state
            .as_ref()
            .and_then(|state| state.track.as_ref())
            .and_then(|track| {
                track
                    .recording_id
                    .map(|recording| player.own(recording, track.rating, None))
            })
            .flatten();
        let track = state.as_ref().and_then(|state| state.track.clone());
        let duration = state
            .as_ref()
            .and_then(|state| state.duration_ms)
            .unwrap_or(0);
        let fraction = if duration > 0 {
            (position as f32 / duration as f32).min(1.)
        } else {
            0.
        };
        let mut animating = false;

        // The facts, and the width they are set at.
        let facts = track.as_ref().and_then(|track| self.facts(track));
        if let (Some(track), Some(facts)) = (&track, &facts) {
            self.refit(track, facts, &sizes, (width, height), window);
        }
        if let Some(track) = &track {
            self.want_picture(track, (width, height), window, cx);
        }

        // A fact giving way: once gone, the next comes up.
        if let Some((start, to)) = self.stepping
            && since(start, now) >= FADE_MS
        {
            self.at = to;
            self.stepping = None;
        }
        let count = facts.as_ref().map_or(0, Vec::len);
        let shown_at = if count > 0 { self.at % count } else { 0 };
        let fact = facts
            .as_ref()
            .and_then(|facts| facts.get(shown_at))
            .cloned();
        let fact_key = match (&fact, &self.read) {
            (Some(fact), Some(read)) => format!("{shown_at}:{:?}:{}", fact.kind, read.appearance),
            _ => String::new(),
        };
        if fact_key != self.fact_key {
            self.fact_key = fact_key;
            self.fact_since = now;
        }
        if self.dot.0 != shown_at {
            self.dot = (shown_at, Some(now));
        }

        // Coming and going.
        let text = sizes.text(self.fit);
        let edge_now = sizes.edge(width, text);
        let edge = match self.glide {
            Some((from, start)) if !still => {
                let t = progress(since(start, now), GLIDE_MS);
                if t >= 1. {
                    self.glide = None;
                } else {
                    animating = true;
                }
                from + (edge_now - from) * arrive_curve(t)
            }
            _ => edge_now,
        };
        self.shown_edge = Some(edge);
        let card_height = height - FOOT - FOOT_PAD - HAIRLINE;
        let cover_top = (card_height - 0.04 * height - sizes.cover) / 2.;
        let cover_bounds = Bounds::new(
            gpui::point(px(edge), px(cover_top)),
            gpui::size(px(sizes.cover), px(sizes.cover)),
        );
        self.cover_at = Some(cover_bounds);
        let bar_cover = crate::bar::cover_bounds(height);
        let top = self.pictures.last();
        if let Stage::Waiting { asked } = self.stage {
            animating = true;
            let waited = now.saturating_duration_since(asked);
            let has_large = top.is_none_or(|top| {
                top.large.is_some() || track.as_ref().is_some_and(|track| track.art_id.is_none())
            });
            if (has_large || waited >= WAIT_COVER) && (self.fit.is_some() || waited >= WAIT_FIT) {
                crate::debug(format_args!(
                    "lean: comes up after {:.0} ms, the cover {}",
                    waited.as_secs_f32() * 1000.,
                    if has_large { "in hand" } else { "late" }
                ));
                self.stage = Stage::Arriving {
                    start: now,
                    from: bar_cover,
                };
                cx.emit(LeanEvent::BarCover(true));
            }
        }
        let (opacity, flying) = match self.stage {
            Stage::Waiting { .. } => (0., None),
            Stage::Arriving { start, from } => {
                let ms = since(start, now);
                animating = true;
                if ms >= OPEN_MS {
                    crate::debug(format_args!("lean: up in {ms:.0} ms"));
                    self.stage = Stage::Open;
                    cx.emit(LeanEvent::BarCover(false));
                    (1., None)
                } else {
                    // The copy holds where the cover rests until the view is up.
                    let t = progress(ms, crate::moves::FLY_MS);
                    let copy = top.map(|top| {
                        flight(
                            from,
                            cover_bounds,
                            t,
                            (8., 16.),
                            (ring_shadow, cover_shadow),
                            top.soft,
                            top.fill.as_ref(),
                            top.large.as_ref(),
                        )
                    });
                    (ease_out(progress(ms, OPEN_MS)), copy)
                }
            }
            Stage::Open => (1., None),
            Stage::Leaving { start, from } => {
                let ms = since(start, now);
                animating = true;
                if ms >= CLOSE_MS {
                    crate::debug(format_args!("lean: gone in {ms:.0} ms"));
                    cx.emit(LeanEvent::Gone);
                }
                let t = progress(ms, crate::moves::FLY_MS);
                let copy = top.map(|top| {
                    flight(
                        from,
                        bar_cover,
                        t,
                        (16., 8.),
                        (cover_shadow, ring_shadow),
                        top.soft,
                        top.fill.as_ref(),
                        top.large.as_ref(),
                    )
                });
                (1. - ease_in(progress(ms, CLOSE_MS)), copy)
            }
        };
        let cover_hidden = flying.is_some();

        let ink = theme::ink();
        let with = |color: gpui::Rgba, alpha: f32| gpui::Rgba {
            a: color.a * alpha,
            ..color
        };
        let mut layers: Vec<AnyElement> = Vec::new();

        // The cover, blurred out of all detail and a little larger than the
        // window, drifting slowly: one way over 44 seconds and back, from
        // `scale(1.05) translate(-1.5%, -1%)` to `scale(1.22) translate(2%, 1.5%)`.
        let (drift_w, drift_h) = (width + 180., height + 180.);
        let side = drift_w.max(drift_h);
        let drifting = !still && playing;
        if drifting {
            if let Some(since) = self.drift_since {
                self.drift_elapsed += now.saturating_duration_since(since);
            }
            self.drift_since = Some(now);
        } else {
            self.drift_since = None;
        }
        let (scale, shift) = if still {
            (1., (0., 0.))
        } else {
            let cycles = self.drift_elapsed.as_secs_f32() * 1000. / DRIFT_MS;
            let local = cycles.fract();
            let along = if (cycles as u64).is_multiple_of(2) {
                local
            } else {
                1. - local
            };
            let p = crate::shell::bezier(0.42, 0., 0.58, 1., along);
            let scale = 1.05 + (1.22 - 1.05) * p;
            let tx = -0.015 + (0.02 + 0.015) * p;
            let ty = -0.01 + (0.015 + 0.01) * p;
            (scale, (tx * drift_w * scale, ty * drift_h * scale))
        };
        let drawn = side * scale;
        let light = !theme::dark();
        for picture in &self.pictures {
            let Some((ground, _)) = &picture.ground else {
                continue;
            };
            let up = match picture.since {
                Some(at) if !still && self.pictures.len() > 1 => {
                    let t = progress(since(at, now), crate::moves::TINT_MS);
                    animating |= t < 1.;
                    ease(t)
                }
                _ => 1.,
            };
            layers.push(
                img(Arc::clone(ground))
                    .absolute()
                    .left(px(width / 2. + shift.0 - drawn / 2.))
                    .top(px(height / 2. + shift.1 - drawn / 2.))
                    .size(px(drawn))
                    .object_fit(ObjectFit::Cover)
                    .opacity(if light { up * 0.8 } else { up })
                    .into_any_element(),
            );
        }
        // The colours under the last go once it is up.
        if let Some(last) = self.pictures.last()
            && self.pictures.len() > 1
            && last
                .since
                .is_none_or(|at| still || since(at, now) >= crate::moves::TINT_MS)
        {
            let keep = self.pictures.len() - 1;
            for old in self.pictures.drain(..keep) {
                if let Some(fill) = old.fill {
                    crate::covers::drop_unshared(fill, Some(window), cx);
                }
                if let Some(large) = old.large {
                    crate::covers::drop_unshared(large, Some(window), cx);
                }
                if let Some((ground, _)) = old.ground {
                    crate::covers::drop_unshared(ground, Some(window), cx);
                }
            }
        }
        // A wash of the window's colour keeps the type readable over any
        // cover, light or dark: thicker behind the words than the cover.
        let bg = theme::bg();
        layers.push(
            div()
                .absolute()
                .size_full()
                .bg(linear_gradient(
                    100.,
                    linear_color_stop(with(bg, 0.78), 0.),
                    linear_color_stop(with(bg, 0.46), 1.),
                ))
                .into_any_element(),
        );

        // The row of the window's own buttons: it moves the window, and
        // holds the way out at the right-hand margin.
        let lean = cx.entity().downgrade();
        let closer = div()
            .id("lean-close")
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(30.))
            .px(px(12.))
            .rounded_full()
            .bg(with(ink, 0.1))
            .text_color(theme::dim())
            .text_size(px(12.5))
            .font_weight(FontWeight(500.))
            .hover(theme::hovered_ink)
            .active(theme::pressed)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                let _ = lean.update(cx, |lean, cx| lean.close(cx));
            })
            .child(
                div()
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(with(ink, 0.12))
                    .text_size(px(11.))
                    .child("esc"),
            )
            .child("Close");
        layers.push(
            div()
                .id("lean-top")
                .absolute()
                .top_0()
                .left(px(edge))
                .w(px(width - 2. * edge))
                .h(px(TOP))
                .flex()
                .items_center()
                .justify_end()
                .window_control_area(WindowControlArea::Drag)
                .when(crate::platform::CHROME.app_moves_window, |row| {
                    row.on_mouse_down(gpui::MouseButton::Left, |event, window, _| {
                        if event.click_count == 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    })
                })
                .child(closer)
                .into_any_element(),
        );

        if let Some(track) = &track {
            // The cover: the pictures of the records played, the last over
            // the ones before as it comes up.
            let mut faces: Vec<AnyElement> = Vec::new();
            let local = Bounds::new(gpui::point(px(0.), px(0.)), cover_bounds.size);
            for picture in &self.pictures {
                let up = match picture.since {
                    Some(at) if !still && self.pictures.len() > 1 => {
                        let t = progress(since(at, now), FACE_MS);
                        animating |= t < 1.;
                        ease(t)
                    }
                    _ => 1.,
                };
                faces.push(face(
                    picture.soft,
                    picture.fill.as_ref(),
                    picture.large.as_ref(),
                    local,
                    16.,
                    up,
                ));
            }
            layers.push(
                div()
                    .absolute()
                    .left(px(edge))
                    .top(px(cover_top))
                    .size(px(sizes.cover))
                    .rounded(px(16.))
                    .bg(theme::fill())
                    .shadow(vec![cover_shadow(1., 1.)])
                    .children(faces)
                    .when(cover_hidden, |cover| cover.invisible())
                    .into_any_element(),
            );

            // The words beside it: the record and its artist, one fact,
            // and a dot for each.
            let mut words = div()
                .absolute()
                .left(px(edge + sizes.cover + sizes.gap))
                .top(px(cover_top + 8.))
                .w(px(text))
                .flex()
                .flex_col();
            let kick = kicker(track);
            let kick_width = tracked(
                kick.clone(),
                14.,
                line(KICK * 1.4),
                FontWeight(600.),
                with(ink, 0.7),
                0.01,
            )
            .width(window);
            let kick_line = line(KICK * 1.4);
            words = words.child(
                div()
                    .mb(px(18.))
                    .h(px(kick_line))
                    .child(if kick_width <= text {
                        tracked(
                            kick,
                            KICK,
                            kick_line,
                            FontWeight(600.),
                            with(ink, 0.7),
                            0.01,
                        )
                        .into_any_element()
                    } else {
                        div()
                            .truncate()
                            .text_size(px(KICK))
                            .line_height(px(kick_line))
                            .font_weight(FontWeight(600.))
                            .text_color(with(ink, 0.7))
                            .child(kick)
                            .into_any_element()
                    }),
            );
            if let Some(fact) = &fact {
                let set = self.set(fact, &sizes, text, window);
                let rise = if still {
                    (1., 1.)
                } else {
                    let ms = since(self.fact_since, now);
                    animating |= ms < RISE_MS + RISE_DELAY_MS;
                    (
                        arrive_curve(progress(ms, RISE_MS)),
                        ease(progress(ms - RISE_DELAY_MS, RISE_MS)),
                    )
                };
                let giving_way = match self.stepping {
                    Some((start, _)) => {
                        animating = true;
                        1. - ease_in(progress(since(start, now), FADE_MS))
                    }
                    None => 1.,
                };
                // What a fact turns on carries the weight, and the rest steps back.
                let rest = with(ink, if light { 0.72 } else { 0.66 });
                let mut head = div().flex().flex_col().mb(px(sizes.below)).opacity(rise.0);
                for (bytes, runs) in &set.lines {
                    let Some(words) = set.head.get(bytes.clone()) else {
                        continue;
                    };
                    let runs = runs
                        .iter()
                        .map(|(len, strong)| (*len, Hsla::from(if *strong { ink } else { rest })))
                        .collect();
                    head = head.child(tracked_runs(
                        words.to_owned(),
                        runs,
                        set.size,
                        line(set.size * 1.04),
                        FontWeight(700.),
                        set.tracking,
                    ));
                }
                let mut said = div().flex().flex_col().opacity(giving_way).child(head);
                if !set.more.is_empty() {
                    said = said.child(
                        div()
                            .mb(px(26.))
                            .opacity(rise.1)
                            .text_size(px(sizes.more))
                            .line_height(px(line(sizes.more * 1.45)))
                            .text_color(with(ink, 0.74))
                            .children(
                                set.more
                                    .iter()
                                    .map(|line| div().whitespace_nowrap().child(line.clone())),
                            ),
                    );
                }
                words = words.child(said);
            }
            if count > 1 {
                let lit = match self.dot.1 {
                    Some(at) if !still => {
                        let t = progress(since(at, now), DOT_MS);
                        animating |= t < 1.;
                        ease(t)
                    }
                    _ => 1.,
                };
                let off = with(ink, 0.22);
                words = words.child(div().flex().gap(px(5.)).children((0..count).map(|index| {
                    let color = if index == shown_at {
                        theme::mix(off, ink, lit)
                    } else {
                        off
                    };
                    div().w(px(16.)).h(px(3.)).rounded(px(2.)).bg(color)
                })));
            }
            layers.push(words.into_any_element());

            // A player's controls along the foot, the song with its stars,
            // and what comes next at the far end.
            let act = |doing: fn(&mut Player, &mut Context<Player>)| {
                let player = self.player.clone();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                    player.update(cx, doing);
                }
            };
            let skip = |id: &'static str, name: &'static str| {
                div()
                    .id(id)
                    .size(px(36.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    // Over the picture, a ground of the ink's own.
                    .hover(|style| style.bg(with(theme::ink(), 0.12)))
                    .active(theme::pressed)
                    .child(icon(name, 16., ink))
            };
            let foot_width = width - 2. * edge;
            let recording = track.recording_id;
            let said = track.rating;
            let stars = crate::marks::stars(
                "lean".into(),
                stars_of(own, track.loved),
                self.star,
                recording.is_some(),
                16.,
                cx,
                |lean: &mut Self, star, over, _| {
                    if over {
                        lean.star = Some(star);
                    } else if lean.star == Some(star) {
                        lean.star = None;
                    }
                },
                move |lean: &mut Self, star, cx| {
                    let Some(recording) = recording else { return };
                    lean.player.update(cx, |player, cx| {
                        player.click_star(recording, said, None, star, cx);
                    });
                },
            );
            let artist = track.artist.as_deref().unwrap_or("Unknown artist");
            let mut said = format!("{artist} · {}", clock(position));
            if duration > 0 {
                said.push_str(&format!(" of {}", clock(duration)));
            }
            let row = |text: SharedString, strong: bool| {
                let row = div().truncate();
                if strong {
                    row.text_size(px(17.))
                        .line_height(px(line(17. * 1.4)))
                        .font_weight(FontWeight(650.))
                        .child(text)
                } else {
                    row.text_size(px(13.5))
                        .line_height(px(line(13.5 * 1.4)))
                        .text_color(theme::dim())
                        .font_features(tabular())
                        .child(text)
                }
            };
            let mut foot = div()
                .absolute()
                .left(px(edge))
                .top(px(height - HAIRLINE - FOOT_PAD - FOOT))
                .w(px(foot_width))
                .h(px(FOOT))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    skip("lean-previous", "previous").on_click(act(|player, cx| {
                        player.skip(SkipDirection::Previous, cx);
                    })),
                )
                .child(
                    div()
                        .id("lean-play")
                        .size(px(46.))
                        .flex_none()
                        .rounded_full()
                        .bg(ink)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(if playing { "pause" } else { "play" }, 16., bg))
                        .hover(theme::hovered_solid)
                        .active(theme::pressed)
                        .on_click(act(|player, cx| player.toggle(cx))),
                )
                .child(skip("lean-next", "next").on_click(act(|player, cx| {
                    player.skip(SkipDirection::Next, cx);
                })))
                .child(
                    div()
                        .flex_shrink(1.)
                        .min_w_0()
                        .ml(px(8.))
                        .flex()
                        .flex_col()
                        .child(row(
                            track
                                .title
                                .clone()
                                .unwrap_or_else(|| "Untitled".to_owned())
                                .into(),
                            true,
                        ))
                        .child(row(said.into(), false)),
                )
                .child(div().flex_none().mx(px(20.)).child(stars));
            if let Some(next) = &self.next {
                foot = foot.child(
                    div()
                        .min_w_0()
                        .max_w(px(foot_width * 0.34))
                        .ml_auto()
                        .flex()
                        .flex_col()
                        .items_end()
                        .child(row("Next".into(), false))
                        .child(row(
                            next.track
                                .title
                                .clone()
                                .unwrap_or_else(|| "Untitled".to_owned())
                                .into(),
                            true,
                        )),
                );
            }
            layers.push(foot.into_any_element());
            // How far through the song, as a hairline across the whole foot.
            layers.push(
                div()
                    .absolute()
                    .left_0()
                    .bottom_0()
                    .w_full()
                    .h(px(HAIRLINE))
                    .bg(with(ink, 0.14))
                    .child(div().h_full().w(px(width * fraction)).bg(ink))
                    .into_any_element(),
            );
        }

        // The drift moves under a fifth of a point at thirty frames a
        // second, so while nothing else moves it is drawn that often rather
        // than at the display's rate, and paused when playback is paused.
        if animating {
            self.drift_tick = None;
            window.request_animation_frame();
        } else if !still && playing {
            if self.drift_tick.is_none() {
                self.drift_tick = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(33))
                        .await;
                    this.update(cx, |lean, cx| {
                        lean.drift_tick = None;
                        cx.notify();
                    })
                    .ok();
                }));
            }
        } else {
            self.drift_tick = None;
        }

        div()
            .absolute()
            .size_full()
            .child(
                div()
                    .id("lean")
                    .absolute()
                    .size_full()
                    .occlude()
                    .overflow_hidden()
                    .bg(bg)
                    .opacity(opacity)
                    .children(layers),
            )
            .children(flying)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> TrackSummary {
        TrackSummary {
            appearance_id: Id::new_v7(1, [0; 10]),
            recording_id: None,
            release_id: None,
            title: Some("Paper Weather".to_owned()),
            artist: None,
            album: None,
            disc_number: None,
            track_number: None,
            duration_ms: None,
            art_id: None,
            rating: None,
            loved: false,
        }
    }

    fn song() -> TrackSummary {
        TrackSummary {
            artist: Some("Hollow Fountain".to_owned()),
            album: Some("The Violet Delta".to_owned()),
            ..bare()
        }
    }

    fn found() -> Fact {
        let phrase = |text: &str, strong| Phrase {
            text: text.to_owned(),
            strong,
        };
        Fact {
            kind: FactKind::FirstHeard,
            head: vec![
                phrase("You first heard this record ", false),
                phrase("19 years ago", true),
                phrase(".", false),
            ],
            more: Some("10 October 2007. 541 plays since.".to_owned()),
        }
    }

    #[test]
    fn a_song_on_the_same_record_does_not_inherit_the_last_songs_facts() {
        let mut track = song();
        let read = Read {
            appearance: track.appearance_id,
            facts: Some(vec![found()]),
            revision: 1,
        };
        assert_eq!(read.for_track(&track), Some(vec![found()]));
        track.appearance_id = Id::from_bytes([4; 16]);
        assert_eq!(read.for_track(&track), None);
    }

    #[test]
    fn says_the_servers_facts_or_the_songs_title_and_artist_when_there_are_none() {
        assert_eq!(facts_for(&song(), Some(&[found()])), vec![found()]);
        let plain = vec![Fact {
            kind: FactKind::SongPlays,
            head: vec![Phrase {
                text: "Paper Weather".to_owned(),
                strong: true,
            }],
            more: Some("Hollow Fountain".to_owned()),
        }];
        assert_eq!(facts_for(&song(), None), plain);
        assert_eq!(facts_for(&song(), Some(&[])), plain);
    }

    #[test]
    fn names_the_record_and_its_artist_over_the_facts() {
        assert_eq!(kicker(&song()), "The Violet Delta · Hollow Fountain");
        let artist = TrackSummary {
            artist: Some("Hollow Fountain".to_owned()),
            ..bare()
        };
        assert_eq!(kicker(&artist), "Now playing");
        let album = TrackSummary {
            album: Some("The Violet Delta".to_owned()),
            ..bare()
        };
        assert_eq!(kicker(&album), "The Violet Delta");
    }

    #[test]
    fn sets_a_long_fact_a_size_smaller() {
        assert!(!long(&found()));
        let longer = Fact {
            head: vec![Phrase {
                text: "x".repeat(61),
                strong: false,
            }],
            ..found()
        };
        assert!(long(&longer));
    }

    #[test]
    fn turns_the_facts_round_both_ways() {
        assert_eq!(stepped(0, 1, 3), 1);
        assert_eq!(stepped(2, 1, 3), 0);
        assert_eq!(stepped(0, -1, 3), 2);
        assert_eq!(stepped(0, 1, 0), 0);
    }

    #[test]
    fn sizes_use_base_dimensions_up_to_about_1680_across_and_grow_past_it() {
        let sizes = Sizes::of(1400., 900.);
        assert_eq!(
            (sizes.side, sizes.h, sizes.cover, sizes.gap),
            (72., 60., 440., 80.)
        );
        assert_eq!((sizes.measure, sizes.avail), (750., 736.));
        assert_eq!(sizes.text(None), 736.);
        assert_eq!(sizes.text(Some(600.)), 600.);
        assert_eq!(sizes.edge(1400., 600.), 140.);
        let large = Sizes::of(2000., 1200.);
        assert_eq!((large.h, large.cover), (72., 600.));
        // A short window: smaller type and a smaller cover, not an overlap.
        let short = Sizes::of(1100., 560.);
        assert!(short.h < 50. && short.cover < 320.);
    }

    #[test]
    fn breaks_a_head_into_lines_of_about_equal_width() {
        // Words of ten points, with a point between: "aaa bbb ccc ddd e".
        let widths = [10., 10., 10., 10., 4.];
        let measure = |range: Range<usize>| {
            let words = widths.get(range.clone()).unwrap_or(&[]);
            words.iter().sum::<f32>() + (words.len().saturating_sub(1)) as f32
        };
        assert_eq!(greedy(5, 40., &measure), vec![0..3, 3..5]);
        // Balanced, the two lines are nearer one width than 32 and 15.
        assert_eq!(balanced(5, 40., &measure), vec![0..2, 2..5]);
        assert_eq!(balanced(5, 100., &measure), vec![0..5]);
        // A word wider than the line is a line of its own.
        assert_eq!(greedy(2, 5., &measure), vec![0..1, 1..2]);
    }

    #[test]
    fn finds_the_words_between_the_spaces() {
        let text = "You first  heard.";
        let found = words(text);
        assert_eq!(found, vec![0..3, 4..9, 11..17]);
        assert_eq!(span(&found, &(1..3)), 4..17);
    }
}
