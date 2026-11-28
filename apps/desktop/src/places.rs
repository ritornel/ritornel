//! Where the music plays: this computer, or another place the library can
//! play on ("Play on" in docs/internals/desktop.md).
//!
//! A hub lists its places (`players.list`), and a server that has the
//! `players` capability says when they change. Choosing a place in the bar's
//! picker moves the music there with `playback.move` (the song, where it is
//! in it, and Up next), or with nothing playing takes the place over as it
//! is, and from then on the player (`player.rs`) follows that place. The
//! choice is forgotten when the app starts again, unless that place is
//! still playing.
//!
//! The picker lists this computer first, then the places that can be
//! reached, by name, then those that cannot, dimmed. Its rows keep their
//! order while it is open, and change in place.

use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Entity, Subscription, Task};
use ritornel_client::{Error, idempotency_key};
use ritornel_contract::methods::{PlaybackMove, PlayersListCall};
use ritornel_contract::{
    Id, NoParams, PlaybackMoveParams, PlaybackState, PlaybackStatus, PlayerInfo, Topic,
};

use crate::link::{Link, LinkEvent};
use crate::net::Net;
use crate::player::Player;
use crate::settings::words::{Clock, when};

/// How far ⌥↑ and ⌥↓ move the volume.
pub(crate) const VOLUME_STEP: f64 = 0.05;

/// When the bar shows a volume slider, as remembered (`remember.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeControl {
    /// For another place only: this computer has its own volume.
    Remote,
    /// For this computer too.
    Always,
    /// Never: the bar names the place alone.
    Never,
}

impl VolumeControl {
    /// Each, in the order Settings lists them.
    pub(crate) const ALL: [Self; 3] = [Self::Remote, Self::Always, Self::Never];

    pub(crate) fn named(name: &str) -> Self {
        match name {
            "always" => Self::Always,
            "never" => Self::Never,
            _ => Self::Remote,
        }
    }

    /// Its name in `remembered.json`.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    /// What Settings calls it.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Remote => "Other places only",
            Self::Always => "Always",
            Self::Never => "Never",
        }
    }
}

/// What the bar's widget shows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Look {
    /// Nothing: there is nowhere else to play, and no slider.
    Empty,
    /// This computer: the speaker icon when there are other places, and a
    /// slider led by a volume icon under `always`.
    Here { icon: bool, slider: Option<f64> },
    /// Another place, lit: its name, and its volume under it unless the
    /// control is `never` or its volume is not known yet. In trouble, what
    /// is wrong, and the slider stops answering.
    There {
        name: String,
        slider: Option<f64>,
        trouble: Option<Trouble>,
    },
}

impl Look {
    /// Whether a slider is shown that answers: ⌥↑ and ⌥↓ then move it.
    pub(crate) fn adjustable(&self) -> bool {
        match self {
            Self::Here { slider, .. } => slider.is_some(),
            Self::There {
                slider, trouble, ..
            } => slider.is_some() && trouble.is_none(),
            Self::Empty => false,
        }
    }
}

/// What is wrong with the place played on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Trouble {
    /// It cannot be reached, since then (Unix milliseconds).
    Away { since_ms: i64 },
    /// Its output stopped working, since then, in the system's words.
    Output { since_ms: i64, detail: String },
}

impl Trouble {
    /// The two lines under the title, short, since the widget names the
    /// place.
    pub(crate) fn lines(&self, clock: &Clock) -> (String, String) {
        match self {
            Self::Away { since_ms } => (
                format!("Out of reach since {}", since(*since_ms, clock)),
                "It plays on by itself".to_owned(),
            ),
            Self::Output { since_ms, .. } => (
                format!("Speakers not working since {}", since(*since_ms, clock)),
                "Check they’re on, then press Play".to_owned(),
            ),
        }
    }

    /// Its colour: a place out of reach waits on it coming back, and one
    /// whose output failed has gone wrong.
    pub(crate) fn status(&self) -> crate::theme::Status {
        match self {
            Self::Away { .. } => crate::theme::Status::Need,
            Self::Output { .. } => crate::theme::Status::Fail,
        }
    }

    /// Its mark: a ring while the place is out of reach, since it comes and
    /// goes, and a dot while it is there with its output failed.
    pub(crate) fn mark(&self) -> crate::marks::Mark {
        match self {
            Self::Away { .. } => crate::marks::Mark::away(self.status()),
            Self::Output { .. } => crate::marks::Mark::here(self.status()),
        }
    }

    /// The whole sentence, as hovering the widget shows it, with the
    /// system's own words for it.
    pub(crate) fn sentence(&self, clock: &Clock) -> String {
        match self {
            Self::Away { since_ms } => format!(
                "Out of reach since {}. It plays on by itself.",
                since(*since_ms, clock)
            ),
            Self::Output { since_ms, detail } => {
                let detail = detail.trim();
                if detail.is_empty() {
                    format!("Speakers not working since {}.", since(*since_ms, clock))
                } else {
                    format!(
                        "Speakers not working since {}: {detail}.",
                        since(*since_ms, clock)
                    )
                }
            }
        }
    }
}

/// Since when, as a person reads it: a time today, or a day and a time.
pub(crate) fn since(ms: i64, clock: &Clock) -> String {
    let at = when(ms, clock);
    match at.strip_prefix("today at ") {
        Some(time) => time.to_owned(),
        None => at,
    }
}

/// What is wrong with `row`, played on in `state`, if anything.
pub(crate) fn trouble(row: &PlayerInfo, state: Option<&PlaybackState>) -> Option<Trouble> {
    if !row.reachable {
        return Some(Trouble::Away {
            since_ms: row.since_ms,
        });
    }
    let problem = state
        .and_then(|state| state.output.problem.as_ref())
        .or_else(|| {
            row.output
                .as_ref()
                .and_then(|output| output.problem.as_ref())
        })?;
    Some(Trouble::Output {
        since_ms: problem.since,
        detail: problem.detail.clone(),
    })
}

/// What the widget shows. `places` is empty where the server lists none;
/// `chosen` is the place played on, or `None` for this computer, and
/// `state` that player's state.
pub(crate) fn look(
    places: &[PlayerInfo],
    chosen: Option<&PlayerInfo>,
    control: VolumeControl,
    state: Option<&PlaybackState>,
) -> Look {
    let volume = state.map(|state| if state.muted { 0. } else { state.volume });
    match chosen {
        Some(row) => Look::There {
            name: row.name.clone(),
            slider: volume.filter(|_| control != VolumeControl::Never),
            trouble: trouble(row, state),
        },
        None => {
            let icon = places.iter().any(|row| !row.is_local);
            let slider = volume.filter(|_| control == VolumeControl::Always);
            if icon || slider.is_some() {
                Look::Here { icon, slider }
            } else {
                Look::Empty
            }
        }
    }
}

/// How loud the volume icon says it is: no wave when silent, one up to a
/// third, two above.
pub(crate) fn waves(volume: f64) -> u8 {
    if volume <= 0. {
        0
    } else if volume <= 1. / 3. {
        1
    } else {
        2
    }
}

/// The picker's order: this computer first, then the places that can be
/// reached, by name, then those that cannot.
pub(crate) fn order(places: &[PlayerInfo]) -> Vec<Id> {
    let mut sorted: Vec<&PlayerInfo> = places.iter().collect();
    sorted.sort_by(|a, b| {
        (
            !a.is_local,
            !a.reachable,
            a.name.to_lowercase(),
            a.player_id,
        )
            .cmp(&(
                !b.is_local,
                !b.reachable,
                b.name.to_lowercase(),
                b.player_id,
            ))
    });
    sorted.into_iter().map(|row| row.player_id).collect()
}

/// What a row says under its name: what is true now.
pub(crate) fn line(row: &PlayerInfo, clock: &Clock) -> String {
    if !row.reachable {
        return format!("Away since {}", since(row.since_ms, clock));
    }
    if let Some(problem) = row
        .output
        .as_ref()
        .and_then(|output| output.problem.as_ref())
    {
        return format!("Speakers not working since {}", since(problem.since, clock));
    }
    let titled = |what: &str| match row.title.as_deref() {
        Some(title) if !title.is_empty() => format!("{what} · {title}"),
        _ => what.to_owned(),
    };
    match row.status {
        Some(PlaybackStatus::Playing | PlaybackStatus::Buffering) => titled("Playing"),
        Some(PlaybackStatus::Paused) => titled("Paused"),
        _ => "Idle".to_owned(),
    }
}

/// Whether a row plays now, for its moving bars.
pub(crate) fn plays(row: &PlayerInfo) -> bool {
    row.reachable
        && matches!(
            row.status,
            Some(PlaybackStatus::Playing | PlaybackStatus::Buffering)
        )
}

/// Where a restart plays: the place remembered, if it is still playing.
pub(crate) fn restore(remembered: Option<Id>, places: &[PlayerInfo]) -> Option<Id> {
    let id = remembered?;
    places
        .iter()
        .find(|row| row.player_id == id && !row.is_local)
        .filter(|row| plays(row))
        .map(|row| row.player_id)
}

/// The picker while it is open: its rows in the order they had when it
/// opened, and the one the keys are on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Picker {
    pub rows: Vec<Id>,
    pub at: Option<Id>,
}

impl Picker {
    /// Opens on `places`, the keys on the place chosen.
    pub(crate) fn open(places: &[PlayerInfo], chosen: Option<Id>) -> Self {
        let rows = order(places);
        let at = chosen.filter(|chosen| rows.contains(chosen));
        Self { rows, at }
    }

    /// Takes a new list in place: rows that are gone leave, new ones join
    /// at the end, and none moves.
    pub(crate) fn refresh(&mut self, places: &[PlayerInfo]) {
        self.rows
            .retain(|id| places.iter().any(|row| row.player_id == *id));
        let new: Vec<Id> = order(places)
            .into_iter()
            .filter(|id| !self.rows.contains(id))
            .collect();
        self.rows.extend(new);
        if self.at.is_some_and(|at| !self.rows.contains(&at)) {
            self.at = None;
        }
    }

    /// Moves the keys up (`-1`) or down (`1`) to the next row that can be
    /// chosen, stopping at the ends.
    pub(crate) fn step(&mut self, by: i32, places: &[PlayerInfo]) {
        let choosable = |id: &Id| {
            places
                .iter()
                .any(|row| row.player_id == *id && row.reachable)
        };
        let from = self
            .at
            .and_then(|at| self.rows.iter().position(|id| *id == at));
        let (before, after) = match from {
            Some(from) => (
                self.rows.get(..from).unwrap_or_default(),
                self.rows.get(from + 1..).unwrap_or_default(),
            ),
            None => (&[][..], self.rows.as_slice()),
        };
        let next = if by > 0 {
            after.iter().find(|id| choosable(id))
        } else if from.is_some() {
            before.iter().rev().find(|id| choosable(id))
        } else {
            after.iter().rev().find(|id| choosable(id))
        };
        if let Some(next) = next {
            self.at = Some(*next);
        }
    }
}

/// What the picker's keys did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerKey {
    Up,
    Down,
    Choose,
    Close,
}

impl PickerKey {
    /// The key, if the picker takes it: ↑, ↓, Return and Esc, alone.
    pub(crate) fn of(keystroke: &gpui::Keystroke) -> Option<Self> {
        let mods = keystroke.modifiers;
        if mods.platform || mods.control || mods.alt || mods.function || mods.shift {
            return None;
        }
        Some(match keystroke.key.as_str() {
            "up" => Self::Up,
            "down" => Self::Down,
            "enter" => Self::Choose,
            "escape" => Self::Close,
            _ => return None,
        })
    }
}

/// Whether a key moves the volume, and which way: ⌥↑ and ⌥↓ (Ctrl+↑ and
/// Ctrl+↓ off a Mac), beside the keys that skip.
pub(crate) fn volume_key(keystroke: &gpui::Keystroke) -> Option<f64> {
    if keystroke.modifiers != crate::platform::KEYS.skip {
        return None;
    }
    match keystroke.key.as_str() {
        "up" => Some(VOLUME_STEP),
        "down" => Some(-VOLUME_STEP),
        _ => None,
    }
}

/// What a move that failed says. Short: the widget names the place.
fn move_failed(error: &Error, to: &str) -> String {
    match error {
        Error::Call(rpc) if crate::player::reason(rpc) == Some("player_unreachable") => {
            format!("Can’t reach {to} right now")
        }
        _ => format!("Couldn’t move the music to {to}"),
    }
}

/// The places to play, the one chosen and the picker.
pub(crate) struct Places {
    net: Option<Arc<Net>>,
    player: Entity<Player>,
    /// The places as last read; none where the server lists none.
    pub list: Vec<PlayerInfo>,
    /// The last read asked for: an older one that answers late is dropped.
    reading: u64,
    /// The place the music is moving to, while it moves.
    pub moving: Option<Id>,
    /// The move under way.
    move_call: Option<Task<()>>,
    /// A move whose answer was lost with its session, sent again under the
    /// same key on the next: the hub answers a resend with what it did.
    unanswered: Option<Move>,
    pub picker: Option<Picker>,
    /// The place remembered from the last run, until the first list read
    /// says whether it still plays.
    remembered: Option<Id>,
    restored: bool,
    /// The absence of the place played on already said, by when it began.
    away_said: Option<(Id, i64)>,
    /// Says that absence once it has lasted a while.
    away_check: Option<Task<()>>,
    _link: Subscription,
}

impl Places {
    pub(crate) fn new(link: &Entity<Link>, player: Entity<Player>, cx: &mut Context<Self>) -> Self {
        let followed = cx.subscribe(link, |places, _, event: &LinkEvent, cx| match event {
            LinkEvent::Connected(net) => {
                places.net = Some(Arc::clone(net));
                places.read(cx);
                if let Some(unanswered) = places.unanswered.take() {
                    places.send_move(unanswered, true, cx);
                }
            }
            LinkEvent::Restated(net) => {
                // A server that becomes a hub or a satellite lists places
                // from then on; one whose hub comes back lists the hub's.
                if net.players && !places.available() {
                    let client = Arc::clone(&net.client);
                    net.rt()
                        .spawn(async move { client.subscribe(&[Topic::Players]).await });
                }
                places.net = Some(Arc::clone(net));
                if net.players {
                    places.read(cx);
                }
            }
            LinkEvent::Away => {
                places.net = None;
                places.picker = None;
                cx.notify();
            }
            LinkEvent::Server(ritornel_contract::Event::Players) => places.read(cx),
            LinkEvent::Server(_) => {}
        });
        let remembered = crate::remember::load();
        let mut places = Self {
            net: None,
            player,
            list: Vec::new(),
            reading: 0,
            moving: None,
            move_call: None,
            unanswered: None,
            picker: None,
            remembered: remembered.play_on.and_then(|id| id.parse().ok()),
            restored: false,
            away_said: None,
            away_check: None,
            _link: followed,
        };
        if let Some(net) = link.read(cx).net() {
            places.net = Some(net);
            places.read(cx);
        }
        // A volume control chosen in Settings shows at once.
        cx.observe_global::<crate::remember::Looks>(|_, cx| cx.notify())
            .detach();
        places
    }

    /// Whether the server lists places at all.
    pub(crate) fn available(&self) -> bool {
        self.net.as_ref().is_some_and(|net| net.players)
    }

    fn row(&self, id: Id) -> Option<&PlayerInfo> {
        self.list.iter().find(|row| row.player_id == id)
    }

    /// This computer's own row, if its server has a player.
    fn local(&self) -> Option<&PlayerInfo> {
        self.list.iter().find(|row| row.is_local)
    }

    /// The place played on, unless it is this computer.
    pub(crate) fn chosen(&self, cx: &gpui::App) -> Option<&PlayerInfo> {
        self.row(self.player.read(cx).target()?)
    }

    /// Whether a satellite's hub is away: it lists only this computer then.
    fn hub_away(&self) -> Option<i64> {
        let hub = self.net.as_ref()?.hub.as_ref()?;
        (hub.answers == Some(false)).then(|| hub.since_ms.unwrap_or_else(crate::away::now_ms))
    }

    /// The row the picker marks as chosen.
    pub(crate) fn chosen_id(&self, cx: &gpui::App) -> Option<Id> {
        self.player
            .read(cx)
            .target()
            .or_else(|| self.local().map(|row| row.player_id))
    }

    /// What the widget shows now.
    pub(crate) fn look(&self, cx: &gpui::App) -> Look {
        // Through a move the place left's level stays shown, so the widget
        // keeps its shape; it does not answer until the new place reports.
        let player = self.player.read(cx);
        let state = player.state.as_ref();
        let control = crate::remember::looks(cx).volume;
        if !self.available() {
            return look(&[], None, control, state);
        }
        let chosen = self.chosen(cx);
        let mut shown = look(&self.list, chosen, control, state);
        // What is wrong with the place left is not the new place's.
        if player.stale()
            && let (Look::There { trouble, .. }, Some(row)) = (&mut shown, chosen)
        {
            *trouble = self::trouble(row, None);
        }
        shown
    }

    /// Whether the volume answers: a slider is shown that is not in
    /// trouble, no move is under way, and the place has reported since one.
    pub(crate) fn adjustable(&self, cx: &gpui::App) -> bool {
        self.moving.is_none() && !self.player.read(cx).stale() && self.look(cx).adjustable()
    }

    /// Reads the places again.
    fn read(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.net.clone().filter(|net| net.players) else {
            self.list.clear();
            self.picker = None;
            // Nowhere else to play: back to this computer.
            self.player
                .update(cx, |player, cx| player.retarget(None, cx));
            cx.notify();
            return;
        };
        self.reading += 1;
        let asked = self.reading;
        let client = Arc::clone(&net.client);
        let read = net
            .rt()
            .spawn(async move { client.call::<PlayersListCall>(&NoParams {}).await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(list)) = read.await else { return };
            this.update(cx, |places, cx| {
                if asked != places.reading
                    || places
                        .net
                        .as_ref()
                        .is_none_or(|current| !current.same_session(&net))
                {
                    return;
                }
                places.took(list.players, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Takes a list of the places.
    pub(crate) fn took(&mut self, mut list: Vec<PlayerInfo>, cx: &mut Context<Self>) {
        crate::debug(format_args!("places: {} listed", list.len()));
        let hub_away = self.hub_away();
        // Without its hub a satellite lists only itself: the place played
        // on is kept, out of reach since the hub went.
        if let Some(since_ms) = hub_away
            && let Some(target) = self.player.read(cx).target()
            && !list.iter().any(|row| row.player_id == target)
            && let Some(mut row) = self.row(target).cloned()
        {
            if row.reachable {
                row.reachable = false;
                row.since_ms = since_ms;
                row.status = None;
                row.title = None;
            }
            list.push(row);
        }
        self.list = list;
        if let Some(picker) = &mut self.picker {
            picker.refresh(&self.list);
        }
        // What the hub lists decides the restart, not a satellite alone.
        if !self.restored && hub_away.is_none() {
            self.restored = true;
            let to = restore(self.remembered.take(), &self.list);
            if to.is_none() {
                crate::remember::set_play_on(None);
            }
            self.retarget(to, cx);
        }
        // A place no longer listed cannot be played on.
        if let Some(target) = self.player.read(cx).target()
            && self.row(target).is_none()
        {
            self.retarget(None, cx);
        }
        self.follow_reach(cx);
        cx.notify();
    }

    /// Plays on `target` from here on, and remembers it.
    fn retarget(&mut self, target: Option<Id>, cx: &mut Context<Self>) {
        self.retarget_to(target, false, cx);
    }

    /// The same; `moved` when the music has just moved there, so what is
    /// shown stays until the place reports.
    fn retarget_to(&mut self, target: Option<Id>, moved: bool, cx: &mut Context<Self>) {
        if self.player.read(cx).target() != target {
            // What was said of the place left is not followed up.
            self.away_said = None;
            self.away_check = None;
        }
        self.player.update(cx, |player, cx| {
            if moved {
                player.moved_to(target, cx);
            } else {
                player.retarget(target, cx);
            }
        });
        crate::remember::set_play_on(target.map(|id| id.to_string()));
        self.follow_reach(cx);
    }

    /// Holds the clock while the place is out of reach, and says so once
    /// it has been away for a while, and again when it is back.
    fn follow_reach(&mut self, cx: &mut Context<Self>) {
        let away = self
            .chosen(cx)
            .filter(|row| !row.reachable)
            .map(|row| (row.player_id, row.since_ms, row.name.clone()));
        let out = away.is_some();
        self.player.update(cx, |player, cx| {
            if player.out_of_reach != out {
                player.out_of_reach = out;
                // Back in reach: what it did meanwhile is read.
                if !out {
                    player.refresh(cx);
                }
                cx.notify();
            }
        });
        match away {
            Some((id, since_ms, name)) if self.away_said != Some((id, since_ms)) => {
                let now = crate::away::now_ms();
                let left = crate::away::QUIET_MS - now.saturating_sub(since_ms);
                if left <= 0 {
                    self.away_said = Some((id, since_ms));
                    self.away_check = None;
                    self.say(format!("Can’t reach {name}. It plays on by itself."), cx);
                } else if self.away_check.is_none() {
                    let wait = Duration::from_millis(u64::try_from(left).unwrap_or(0));
                    self.away_check = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(wait).await;
                        this.update(cx, |places, cx| {
                            places.away_check = None;
                            places.follow_reach(cx);
                        })
                        .ok();
                    }));
                }
            }
            Some(_) => {}
            None => {
                self.away_check = None;
                if let Some((id, _)) = self.away_said.take()
                    && let Some(row) = self.row(id).filter(|row| row.reachable)
                {
                    let name = row.name.clone();
                    self.say(format!("{name} is back."), cx);
                }
            }
        }
    }

    fn say(&self, words: String, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.say(words, cx));
    }

    /// Opens the picker, or closes it.
    pub(crate) fn toggle_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            self.picker = None;
        } else if self.available() && !self.list.is_empty() {
            let chosen = self.chosen_id(cx);
            self.picker = Some(Picker::open(&self.list, chosen));
            // What it lists is read again as it opens.
            self.read(cx);
        }
        cx.notify();
    }

    pub(crate) fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            cx.notify();
        }
    }

    /// A key while the window has it. Whether it was taken: the picker's
    /// keys while it is open, and the volume's while a slider is shown.
    pub(crate) fn key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        if self.picker.is_some()
            && let Some(key) = PickerKey::of(keystroke)
        {
            match key {
                PickerKey::Up | PickerKey::Down => {
                    let by = if key == PickerKey::Up { -1 } else { 1 };
                    if let Some(picker) = &mut self.picker {
                        picker.step(by, &self.list);
                    }
                    cx.notify();
                }
                PickerKey::Choose => {
                    if let Some(at) = self.picker.as_ref().and_then(|picker| picker.at) {
                        self.choose(at, cx);
                    }
                }
                PickerKey::Close => self.close_picker(cx),
            }
            return true;
        }
        if let Some(by) = volume_key(keystroke)
            && self.adjustable(cx)
        {
            self.nudge(by, cx);
            return true;
        }
        false
    }

    /// Moves the volume by `by`.
    pub(crate) fn nudge(&mut self, by: f64, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| {
            if let Some(volume) = player.state.as_ref().map(|state| state.volume) {
                player.set_volume(volume + by, cx);
            }
        });
    }

    /// Plays on the place `to`: the music moves there, or with nothing
    /// playing the place is taken over as it is. The picker closes.
    pub(crate) fn choose(&mut self, to: Id, cx: &mut Context<Self>) {
        self.close_picker(cx);
        let Some(row) = self.row(to).cloned() else {
            return;
        };
        let target = (!row.is_local).then_some(to);
        let player = self.player.read(cx);
        if self.moving.is_some() || !row.reachable || target == player.target() {
            return;
        }
        // Nothing can be taken from a place out of reach: it is let go
        // as it is, and plays on by itself.
        let from = match player.target() {
            Some(from) => self
                .row(from)
                .filter(|row| row.reachable)
                .map(|row| row.player_id),
            None => self.local().map(|row| row.player_id),
        };
        // Until the place's state is read, what it plays is not known; one
        // kept through a move is the place left's.
        if from.is_some() && (player.state.is_none() || player.stale()) {
            self.say(
                "Still finding out what’s playing. Try again in a moment.".to_owned(),
                cx,
            );
            return;
        }
        let entry = player
            .state
            .as_ref()
            .filter(|state| state.status != PlaybackStatus::Stopped)
            .and_then(|state| state.entry_id);
        let (Some(from), Some(entry_id)) = (from, entry) else {
            self.retarget(target, cx);
            cx.notify();
            return;
        };
        let Ok(key) = idempotency_key() else {
            self.say(format!("Couldn’t move the music to {}", row.name), cx);
            return;
        };
        crate::debug(format_args!("places: moving from {from} to {to}"));
        let movement = Move {
            params: PlaybackMoveParams {
                idempotency_key: key,
                from,
                to,
                entry_id,
            },
            name: row.name,
            target,
        };
        self.send_move(movement, false, cx);
    }

    /// Sends a move, and follows the place once it has the music. A move
    /// whose answer is lost with the session is sent again once, under the
    /// same key, on the next.
    fn send_move(&mut self, movement: Move, resent: bool, cx: &mut Context<Self>) {
        self.moving = Some(movement.params.to);
        self.player.update(cx, |player, _| player.move_began());
        cx.notify();
        let Some(net) = self.net.clone() else {
            self.unanswered = Some(movement);
            return;
        };
        let client = Arc::clone(&net.client);
        let params = movement.params;
        let call = net
            .rt()
            .spawn(async move { client.call::<PlaybackMove>(&params).await });
        self.move_call = Some(cx.spawn(async move |this, cx| {
            let answer = call.await;
            this.update(cx, |places, cx| {
                places.move_call = None;
                match answer {
                    // Moved: the place has the queue, though the session
                    // that asked may have ended since.
                    Ok(Ok(_)) => {
                        places.moving = None;
                        places.retarget_to(movement.target, true, cx);
                    }
                    Ok(Err(Error::Transport(_))) if !resent => {
                        let session = places.net.clone().filter(|now| !now.same_session(&net));
                        match session {
                            Some(_) => places.send_move(movement, true, cx),
                            None => places.unanswered = Some(movement),
                        }
                    }
                    Ok(Err(error)) => {
                        places.moving = None;
                        places
                            .player
                            .update(cx, |player, cx| player.move_failed(cx));
                        places.say(move_failed(&error, &movement.name), cx);
                        places.read(cx);
                    }
                    Err(_) => {
                        places.moving = None;
                        places
                            .player
                            .update(cx, |player, cx| player.move_failed(cx));
                        places.say(format!("Couldn’t move the music to {}", movement.name), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }
}

/// A move of the music to another place, as sent.
struct Move {
    params: PlaybackMoveParams,
    /// The place's name, for what is said if it fails.
    name: String,
    /// The player's target once it has moved.
    target: Option<Id>,
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test fails by saying what it found"
)]
pub(crate) mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Instant;

    use gpui::{AppContext, Keystroke, Modifiers, TestAppContext};
    use ritornel_contract::{
        ErrorCode, Event, Frame, OutputProblem, PlaybackEvent, Response, RpcError, ServerRole,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::settings::words::Style;

    pub(crate) const HERE: Id = Id::from_bytes([1; 16]);
    pub(crate) const LIVING_ROOM: Id = Id::from_bytes([2; 16]);
    pub(crate) const STUDIO: Id = Id::from_bytes([3; 16]);
    pub(crate) const DESK: Id = Id::from_bytes([4; 16]);
    pub(crate) const ENTRY: Id = Id::from_bytes([9; 16]);

    pub(crate) fn place(id: Id, name: &str, local: bool, reachable: bool) -> PlayerInfo {
        PlayerInfo {
            player_id: id,
            name: name.to_owned(),
            server_id: id,
            is_local: local,
            reachable,
            since_ms: 1_794_776_400_000,
            status: reachable.then_some(PlaybackStatus::Stopped),
            title: None,
            output: None,
        }
    }

    fn playing(mut row: PlayerInfo, title: &str) -> PlayerInfo {
        row.status = Some(PlaybackStatus::Playing);
        row.title = Some(title.to_owned());
        row
    }

    pub(crate) fn places() -> Vec<PlayerInfo> {
        vec![
            place(STUDIO, "studio", false, true),
            place(DESK, "Desk", false, false),
            place(LIVING_ROOM, "Living Room", false, true),
            place(HERE, "Workbench", true, true),
        ]
    }

    /// 15 November 2026, 21:00 UTC, and the hour after.
    fn clock() -> Clock {
        Clock {
            now: 1_794_776_400_000 + 3_600_000,
            zone: jiff::tz::TimeZone::UTC,
            style: Style {
                hour12: true,
                month_first: true,
            },
        }
    }

    pub(crate) fn state(volume: f64) -> PlaybackState {
        let Ok(state) = serde_json::from_value(json!({
            "revision": 1, "status": "playing", "entryId": ENTRY.to_string(), "positionMs": 2000,
            "volume": volume, "muted": false, "shuffle": false, "repeat": "off",
            "queueRevision": 1, "queueLength": 1,
            "output": {"name": "null", "mode": "shared", "sampleRateHz": 44100, "channels": 2}
        })) else {
            panic!("an invented state");
        };
        state
    }

    #[test]
    fn the_picker_lists_this_computer_then_reachable_places_by_name_then_the_rest() {
        assert_eq!(order(&places()), [HERE, LIVING_ROOM, STUDIO, DESK]);
    }

    #[test]
    fn rows_change_in_place_and_never_reorder_while_the_picker_is_open() {
        let mut list = places();
        let mut picker = Picker::open(&list, Some(HERE));
        assert_eq!(picker.at, Some(HERE));
        // The studio goes away and a new place comes: nothing moves, the
        // new one joins at the end.
        list[0].reachable = false;
        list.push(place(Id::from_bytes([5; 16]), "Attic", false, true));
        picker.refresh(&list);
        assert_eq!(
            picker.rows,
            [HERE, LIVING_ROOM, STUDIO, DESK, Id::from_bytes([5; 16])]
        );
        // A place that leaves the list leaves the picker, and the keys with it.
        picker.at = Some(DESK);
        list.retain(|row| row.player_id != DESK);
        picker.refresh(&list);
        assert_eq!(
            picker.rows,
            [HERE, LIVING_ROOM, STUDIO, Id::from_bytes([5; 16])]
        );
        assert_eq!(picker.at, None);
    }

    #[test]
    fn the_keys_pass_over_places_that_cannot_be_chosen_and_stop_at_the_ends() {
        let list = places();
        let mut picker = Picker::open(&list, Some(HERE));
        picker.step(-1, &list);
        assert_eq!(picker.at, Some(HERE));
        picker.step(1, &list);
        picker.step(1, &list);
        assert_eq!(picker.at, Some(STUDIO));
        // The desk is away: down stays on the studio.
        picker.step(1, &list);
        assert_eq!(picker.at, Some(STUDIO));
        picker.step(-1, &list);
        assert_eq!(picker.at, Some(LIVING_ROOM));
        // With none chosen, down starts at the top and up at the bottom.
        let mut fresh = Picker::open(&list, None);
        fresh.step(-1, &list);
        assert_eq!(fresh.at, Some(STUDIO));
    }

    #[test]
    fn each_row_says_what_is_true_now() {
        let clock = clock();
        assert_eq!(
            line(
                &playing(place(STUDIO, "Studio", false, true), "Nightjar"),
                &clock
            ),
            "Playing · Nightjar"
        );
        let mut paused = playing(place(STUDIO, "Studio", false, true), "Nightjar");
        paused.status = Some(PlaybackStatus::Paused);
        assert_eq!(line(&paused, &clock), "Paused · Nightjar");
        assert_eq!(line(&place(STUDIO, "Studio", false, true), &clock), "Idle");
        assert_eq!(
            line(&place(DESK, "Desk", false, false), &clock),
            "Away since 9:00 PM"
        );
        let mut yesterday = place(DESK, "Desk", false, false);
        yesterday.since_ms -= 86_400_000;
        assert_eq!(line(&yesterday, &clock), "Away since yesterday at 9:00 PM");
    }

    #[test]
    fn a_restart_goes_back_to_a_place_only_while_it_still_plays() {
        let mut list = places();
        assert_eq!(restore(Some(LIVING_ROOM), &list), None);
        list[2] = playing(list[2].clone(), "Nightjar");
        assert_eq!(restore(Some(LIVING_ROOM), &list), Some(LIVING_ROOM));
        assert_eq!(restore(None, &list), None);
        // Gone, or out of reach, it is forgotten.
        list[2].reachable = false;
        assert_eq!(restore(Some(LIVING_ROOM), &list), None);
        assert_eq!(restore(Some(Id::from_bytes([7; 16])), &list), None);
    }

    #[test]
    fn the_widget_shows_the_icon_here_and_lights_up_with_a_place_elsewhere() {
        let list = places();
        let here = state(0.4);
        // Nowhere else to play: nothing, or the slider under `always`.
        let alone = [place(HERE, "Workbench", true, true)];
        assert_eq!(
            look(&alone, None, VolumeControl::Remote, Some(&here)),
            Look::Empty
        );
        assert_eq!(
            look(&alone, None, VolumeControl::Always, Some(&here)),
            Look::Here {
                icon: false,
                slider: Some(0.4)
            }
        );
        // Other places: the icon, and the slider before it under `always`.
        assert_eq!(
            look(&list, None, VolumeControl::Remote, Some(&here)),
            Look::Here {
                icon: true,
                slider: None
            }
        );
        // Elsewhere: lit, with that place's volume, or its name alone
        // under `never`.
        let there = look(&list, Some(&list[2]), VolumeControl::Remote, Some(&here));
        assert_eq!(
            there,
            Look::There {
                name: "Living Room".to_owned(),
                slider: Some(0.4),
                trouble: None
            }
        );
        assert!(there.adjustable());
        assert_eq!(
            look(&list, Some(&list[2]), VolumeControl::Never, Some(&here)),
            Look::There {
                name: "Living Room".to_owned(),
                slider: None,
                trouble: None
            }
        );
    }

    #[test]
    fn a_place_out_of_reach_is_an_amber_ring_and_a_failed_output_a_red_dot() {
        use crate::marks::Mark;
        use crate::theme::Status;
        let away = Trouble::Away { since_ms: 0 };
        let output = Trouble::Output {
            since_ms: 0,
            detail: String::new(),
        };
        assert_eq!(away.status(), Status::Need);
        assert_eq!(away.mark(), Mark::away(Status::Need));
        assert_eq!(output.status(), Status::Fail);
        assert_eq!(output.mark(), Mark::here(Status::Fail));
    }

    #[test]
    fn a_place_in_trouble_wears_the_warning_and_its_slider_stops_answering() {
        let list = places();
        let away = look(
            &list,
            Some(&list[1]),
            VolumeControl::Remote,
            Some(&state(0.4)),
        );
        let Look::There { trouble, .. } = &away else {
            panic!("not lit: {away:?}");
        };
        assert_eq!(
            trouble,
            &Some(Trouble::Away {
                since_ms: 1_794_776_400_000
            })
        );
        assert!(!away.adjustable());
        let mut broken = state(0.4);
        broken.output.problem = Some(OutputProblem {
            since: 1_794_776_400_000,
            detail: "no device is there".to_owned(),
        });
        let trouble = trouble_of(&list[2], &broken);
        assert_eq!(
            trouble.lines(&clock()),
            (
                "Speakers not working since 9:00 PM".to_owned(),
                "Check they’re on, then press Play".to_owned()
            )
        );
        assert_eq!(
            trouble.sentence(&clock()),
            "Speakers not working since 9:00 PM: no device is there."
        );
        assert_eq!(
            Trouble::Away {
                since_ms: 1_794_776_400_000
            }
            .lines(&clock()),
            (
                "Out of reach since 9:00 PM".to_owned(),
                "It plays on by itself".to_owned()
            )
        );
    }

    fn trouble_of(row: &PlayerInfo, state: &PlaybackState) -> Trouble {
        let Some(trouble) = trouble(row, Some(state)) else {
            panic!("no trouble");
        };
        trouble
    }

    #[test]
    fn the_volume_icon_has_no_wave_when_silent_one_to_a_third_and_two_above() {
        assert_eq!(waves(0.), 0);
        assert_eq!(waves(0.2), 1);
        assert_eq!(waves(1. / 3.), 1);
        assert_eq!(waves(0.5), 2);
    }

    fn key(source: &'static str) -> Keystroke {
        let Ok(key) = Keystroke::parse(source) else {
            panic!("not a key: {source}");
        };
        key
    }

    fn volume(key: &str) -> Keystroke {
        Keystroke {
            modifiers: crate::platform::KEYS.skip,
            key: key.to_owned(),
            key_char: None,
        }
    }

    #[test]
    fn the_picker_takes_the_arrows_return_and_esc_alone_and_the_volume_its_skip_modifier() {
        assert_eq!(PickerKey::of(&key("up")), Some(PickerKey::Up));
        assert_eq!(PickerKey::of(&key("down")), Some(PickerKey::Down));
        assert_eq!(PickerKey::of(&key("enter")), Some(PickerKey::Choose));
        assert_eq!(PickerKey::of(&key("escape")), Some(PickerKey::Close));
        assert_eq!(PickerKey::of(&key("shift-up")), None);
        assert_eq!(volume_key(&volume("up")), Some(VOLUME_STEP));
        assert_eq!(volume_key(&volume("down")), Some(-VOLUME_STEP));
        assert_eq!(volume_key(&volume("left")), None);
        let bare = Keystroke {
            modifiers: Modifiers::default(),
            ..volume("up")
        };
        assert_eq!(volume_key(&bare), None);
    }

    /// A server that answers from `answer`, and what it was asked.
    #[derive(Clone, Default)]
    pub(crate) struct Asked(pub Arc<Mutex<Vec<(String, Value)>>>);

    impl Asked {
        pub(crate) fn all(&self) -> Vec<(String, Value)> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        pub(crate) fn of(&self, method: &str) -> Vec<Value> {
            self.all()
                .into_iter()
                .filter(|(asked, _)| asked == method)
                .map(|(_, params)| params)
                .collect()
        }
    }

    /// A planned answer that hangs up instead.
    const HANG_UP: &str = "hang up";

    pub(crate) type Answers = Arc<Mutex<VecDeque<(String, Result<Value, RpcError>)>>>;

    /// A hub over memory. `players.list` answers `list`, `playback.get` a
    /// playing state; a call named in `answers` takes the first answer for
    /// it; everything else answers a receipt.
    pub(crate) fn hub(list: Vec<PlayerInfo>, answers: Answers) -> (Net, Asked) {
        let (send, mut calls) = tokio::sync::mpsc::channel::<String>(64);
        let (answer, receive) = tokio::sync::mpsc::channel(64);
        let _runtime = crate::net::rt().enter();
        let client = Arc::new(ritornel_client::Client::over_memory(send, receive));
        let asked = Asked::default();
        let heard = asked.clone();
        crate::net::rt().spawn(async move {
            while let Some(text) = calls.recv().await {
                let Ok(Frame::Request(request)) = Frame::decode(&text) else {
                    continue;
                };
                let params = request.params.clone().unwrap_or(Value::Null);
                heard
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((request.method.clone(), params.clone()));
                let planned = {
                    let mut answers = answers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    answers
                        .iter()
                        .position(|(method, _)| *method == request.method)
                        .and_then(|at| answers.remove(at))
                        .map(|(_, outcome)| outcome)
                };
                // A planned hang-up ends the session, its answer lost.
                if matches!(&planned, Some(Ok(Value::String(said))) if said == HANG_UP) {
                    return;
                }
                let outcome = planned.unwrap_or_else(|| match request.method.as_str() {
                    "players.list" => Ok(json!({ "players": list })),
                    "playback.get" => Ok(json!(state(0.4))),
                    "events.subscribe" | "events.unsubscribe" => Ok(json!({
                        "player": params.get("player"),
                        "topics": params.get("topics"),
                    })),
                    "playback.move" => Ok(json!({
                        "commandId": params.get("idempotencyKey"),
                        "status": "done",
                    })),
                    _ => Ok(json!({
                        "commandId": params.get("idempotencyKey"),
                        "status": "done",
                        "queueRevision": 1,
                    })),
                });
                let frame = Frame::Response(Response {
                    id: Some(request.id),
                    outcome,
                });
                if answer.send(frame.encode()).await.is_err() {
                    return;
                }
            }
        });
        let net = Net::test_session(client, ServerRole::Hub).with_players();
        (net, asked)
    }

    pub(crate) struct Rig {
        pub link: Entity<Link>,
        pub player: Entity<Player>,
        pub places: Entity<Places>,
    }

    pub(crate) fn rig(cx: &mut TestAppContext, remembered: Option<Id>) -> Rig {
        cx.executor().allow_parking();
        let link = cx.update(crate::link::unlinked);
        let player = cx.new(|cx| Player::new(&link, cx));
        let places = cx.new(|cx| Places::new(&link, player.clone(), cx));
        places.update(cx, |places, _| places.remembered = remembered);
        Rig {
            link,
            player,
            places,
        }
    }

    pub(crate) fn connect(rig: &Rig, net: Net, cx: &mut TestAppContext) {
        rig.link
            .update(cx, |_, cx| cx.emit(LinkEvent::Connected(Arc::new(net))));
    }

    /// Runs until `done` holds.
    pub(crate) fn until(
        cx: &mut TestAppContext,
        what: &str,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        let deadline = Instant::now() + std::time::Duration::from_secs(60);
        loop {
            assert!(Instant::now() < deadline, "it never {what}");
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            std::thread::yield_now();
        }
    }

    pub(crate) fn target(rig: &Rig, cx: &mut TestAppContext) -> Option<Id> {
        rig.player.read_with(cx, |player, _| player.target())
    }

    pub(crate) fn listed(rig: &Rig, cx: &mut TestAppContext) -> bool {
        rig.places
            .read_with(cx, |places, _| !places.list.is_empty())
            && rig.player.read_with(cx, |player, _| player.state.is_some())
    }

    #[gpui::test]
    fn choosing_a_place_moves_the_music_there_and_follows_it(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, asked) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));

        rig.places
            .update(cx, |places, cx| places.choose(LIVING_ROOM, cx));
        until(cx, "heard from the living room", |cx| {
            target(&rig, cx) == Some(LIVING_ROOM)
                && rig
                    .player
                    .read_with(cx, |player, _| player.state.is_some() && !player.stale())
        });
        let moved = asked.of("playback.move");
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0]["from"], json!(HERE.to_string()));
        assert_eq!(moved[0]["to"], json!(LIVING_ROOM.to_string()));
        assert_eq!(moved[0]["entryId"], json!(ENTRY.to_string()));
        until(cx, "subscribed to the living room", |_| {
            asked
                .of("events.subscribe")
                .iter()
                .any(|params| params["player"] == json!(LIVING_ROOM.to_string()))
        });
        assert!(
            asked
                .of("playback.get")
                .iter()
                .any(|params| params["player"] == json!(LIVING_ROOM.to_string()))
        );

        // From then on, its calls name it.
        rig.player.update(cx, |player, cx| player.toggle(cx));
        until(cx, "paused the living room", |_| {
            !asked.of("playback.pause").is_empty()
        });
        assert_eq!(
            asked.of("playback.pause")[0]["player"],
            json!(LIVING_ROOM.to_string())
        );

        // Its events are followed, and this computer's own pass.
        let mut theirs = state(0.7);
        theirs.revision = 50;
        let mut ours = state(0.1);
        ours.revision = 60;
        rig.link.update(cx, |_, cx| {
            cx.emit(LinkEvent::Server(Event::Playback(Box::new(
                PlaybackEvent {
                    player: None,
                    state: ours,
                },
            ))));
            cx.emit(LinkEvent::Server(Event::Playback(Box::new(
                PlaybackEvent {
                    player: Some(LIVING_ROOM),
                    state: theirs,
                },
            ))));
        });
        let volume = rig.player.read_with(cx, |player, _| {
            player.state.as_ref().map(|state| state.volume)
        });
        assert_eq!(volume, Some(0.7));

        // Back here, the living room's events are let go.
        rig.places.update(cx, |places, cx| places.choose(HERE, cx));
        until(cx, "came back here", |cx| target(&rig, cx).is_none());
        until(cx, "let the living room go", |_| {
            asked
                .of("events.unsubscribe")
                .iter()
                .any(|params| params["player"] == json!(LIVING_ROOM.to_string()))
        });
        let back = asked.of("playback.move");
        assert_eq!(back.len(), 2);
        assert_eq!(back[1]["from"], json!(LIVING_ROOM.to_string()));
        assert_eq!(back[1]["to"], json!(HERE.to_string()));
    }

    #[gpui::test]
    fn a_move_that_fails_says_so_briefly_and_stays_where_it_was(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let refusal = RpcError::new(ErrorCode::Unavailable, "Living Room is away")
            .with_detail("reason", "player_unreachable");
        let answers: Answers = Arc::new(Mutex::new(VecDeque::from([(
            "playback.move".to_owned(),
            Err(refusal),
        )])));
        let (net, asked) = hub(places(), answers);
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        rig.places
            .update(cx, |places, cx| places.choose(LIVING_ROOM, cx));
        // While it moves, the widget stays as it was, and the volume waits.
        let (moving, look) = rig
            .places
            .read_with(cx, |places, cx| (places.moving, places.look(cx)));
        assert_eq!(moving, Some(LIVING_ROOM));
        assert!(matches!(look, Look::Here { .. }), "{look:?}");
        until(cx, "heard the move fail", |cx| {
            rig.places
                .read_with(cx, |places, _| places.moving.is_none())
        });
        assert_eq!(asked.of("playback.move").len(), 1);
        assert_eq!(target(&rig, cx), None);
        let said = rig.player.read_with(cx, |player, _| {
            player.toast.as_ref().map(|(words, _)| words.to_string())
        });
        assert_eq!(said.as_deref(), Some("Can’t reach Living Room right now"));
        assert!(
            asked
                .of("events.subscribe")
                .iter()
                .all(|params| params.get("player").is_none())
        );
    }

    #[gpui::test]
    fn with_nothing_playing_a_place_is_taken_over_as_it_is(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, asked) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        rig.player.update(cx, |player, _| {
            if let Some(state) = &mut player.state {
                state.status = PlaybackStatus::Stopped;
                state.entry_id = None;
            }
        });
        rig.places
            .update(cx, |places, cx| places.choose(STUDIO, cx));
        assert_eq!(target(&rig, cx), Some(STUDIO));
        cx.run_until_parked();
        assert!(asked.of("playback.move").is_empty());
        // A place out of reach cannot be chosen.
        rig.places.update(cx, |places, cx| places.choose(DESK, cx));
        assert_eq!(target(&rig, cx), Some(STUDIO));
    }

    #[gpui::test]
    fn the_keys_move_through_the_picker_and_return_chooses(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, _) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        let press = |source: &'static str, cx: &mut TestAppContext| {
            rig.places
                .update(cx, |places, cx| places.key(&key(source), cx))
        };
        // Closed, the picker takes no keys.
        assert!(!press("down", cx));
        rig.places.update(cx, |places, cx| places.toggle_picker(cx));
        let at = |cx: &mut TestAppContext| {
            rig.places
                .read_with(cx, |places, _| places.picker.as_ref().and_then(|p| p.at))
        };
        assert_eq!(at(cx), Some(HERE));
        assert!(press("down", cx));
        assert!(press("down", cx));
        assert_eq!(at(cx), Some(STUDIO));
        // Esc closes it, and the keys are the window's again.
        assert!(press("escape", cx));
        assert!(
            rig.places
                .read_with(cx, |places, _| places.picker.is_none())
        );
        rig.places.update(cx, |places, cx| places.toggle_picker(cx));
        assert!(press("down", cx));
        assert!(press("enter", cx));
        assert!(
            rig.places
                .read_with(cx, |places, _| places.picker.is_none())
        );
        until(cx, "moved to the living room", |cx| {
            target(&rig, cx) == Some(LIVING_ROOM)
        });
    }

    #[gpui::test]
    fn a_restart_plays_on_the_place_remembered_only_if_it_still_plays(cx: &mut TestAppContext) {
        let mut list = places();
        list[2] = playing(list[2].clone(), "Nightjar");
        let rig_playing = rig(cx, Some(LIVING_ROOM));
        let (net, asked) = hub(list, Answers::default());
        connect(&rig_playing, net, cx);
        until(cx, "went back to the living room", |cx| {
            target(&rig_playing, cx) == Some(LIVING_ROOM)
        });
        assert!(asked.of("playback.move").is_empty());

        let rig_idle = rig(cx, Some(LIVING_ROOM));
        let (net, _) = hub(places(), Answers::default());
        connect(&rig_idle, net, cx);
        until(cx, "read the places", |cx| listed(&rig_idle, cx));
        assert_eq!(target(&rig_idle, cx), None);
        assert!(
            rig_idle
                .places
                .read_with(cx, |places, _| places.remembered.is_none())
        );
    }

    #[gpui::test]
    fn option_up_and_down_move_the_volume_while_a_slider_is_shown(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, asked) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        // Here, under `remote`, there is no slider: the key is the window's.
        assert!(
            !rig.places
                .update(cx, |places, cx| places.key(&volume("up"), cx))
        );
        rig.places
            .update(cx, |places, cx| places.choose(STUDIO, cx));
        until(cx, "heard from the studio", |cx| {
            target(&rig, cx) == Some(STUDIO)
                && rig
                    .player
                    .read_with(cx, |player, _| player.state.is_some() && !player.stale())
        });
        assert!(
            rig.places
                .update(cx, |places, cx| places.key(&volume("up"), cx))
        );
        until(cx, "set the studio's volume", |_| {
            !asked.of("playback.set").is_empty()
        });
        let set = &asked.of("playback.set")[0];
        assert_eq!(set["player"], json!(STUDIO.to_string()));
        let sent = set["volume"].as_f64().unwrap_or_default();
        assert!((sent - 0.45).abs() < 1e-9, "{sent}");
    }

    #[gpui::test]
    fn a_move_whose_answer_is_lost_is_sent_again_under_its_key_on_the_next_session(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, None);
        let answers: Answers = Arc::new(Mutex::new(VecDeque::from([(
            "playback.move".to_owned(),
            Ok(Value::String(HANG_UP.to_owned())),
        )])));
        let (net, first) = hub(places(), answers);
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        rig.places
            .update(cx, |places, cx| places.choose(LIVING_ROOM, cx));
        until(cx, "lost the answer", |cx| {
            rig.places
                .read_with(cx, |places, _| places.unanswered.is_some())
        });
        rig.link.update(cx, |_, cx| cx.emit(LinkEvent::Away));
        let (net, second) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "moved to the living room", |cx| {
            target(&rig, cx) == Some(LIVING_ROOM)
        });
        let (sent, resent) = (first.of("playback.move"), second.of("playback.move"));
        assert_eq!((sent.len(), resent.len()), (1, 1));
        assert_eq!(sent[0]["idempotencyKey"], resent[0]["idempotencyKey"]);
    }

    #[gpui::test]
    fn a_place_is_not_chosen_before_what_plays_here_is_known(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, asked) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        rig.player.update(cx, |player, _| player.state = None);
        rig.places
            .update(cx, |places, cx| places.choose(LIVING_ROOM, cx));
        cx.run_until_parked();
        assert_eq!(target(&rig, cx), None);
        assert!(asked.of("playback.move").is_empty());
    }

    #[gpui::test]
    fn while_a_satellites_hub_is_away_the_place_played_on_stays_out_of_reach(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, None);
        let (net, _) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        rig.places
            .update(cx, |places, cx| places.choose(STUDIO, cx));
        until(cx, "moved to the studio", |cx| {
            target(&rig, cx) == Some(STUDIO)
        });
        // The hub goes: the satellite lists only itself.
        let Some(client) = rig.places.read_with(cx, |places, _| {
            places.net.as_ref().map(|net| Arc::clone(&net.client))
        }) else {
            panic!("no session");
        };
        let away = Net::satellite(client, false).with_players();
        rig.places
            .update(cx, |places, _| places.net = Some(Arc::new(away)));
        let alone = vec![place(HERE, "Workbench", true, true)];
        rig.places.update(cx, |places, cx| places.took(alone, cx));
        assert_eq!(target(&rig, cx), Some(STUDIO));
        let studio = rig
            .places
            .read_with(cx, |places, cx| places.chosen(cx).cloned());
        assert!(studio.is_some_and(|row| !row.reachable));
        assert!(rig.player.read_with(cx, |player, _| player.out_of_reach));
    }

    #[gpui::test]
    fn the_song_shown_stays_through_a_move_until_the_new_place_reports(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, _) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        let shown = |cx: &mut TestAppContext| {
            rig.player.read_with(cx, |player, _| {
                player
                    .state
                    .as_ref()
                    .map(|state| (state.entry_id, state.volume, player.stale()))
            })
        };
        // Moved: nothing blinks out while the living room is asked.
        rig.player
            .update(cx, |player, cx| player.moved_to(Some(LIVING_ROOM), cx));
        assert_eq!(shown(cx), Some((Some(ENTRY), 0.4, true)));
        // Its slider keeps its place, and waits for the living room's own volume.
        let (look, adjustable) = rig
            .places
            .read_with(cx, |places, cx| (places.look(cx), places.adjustable(cx)));
        assert!(
            matches!(
                look,
                Look::There {
                    slider: Some(_),
                    ..
                }
            ),
            "{look:?}"
        );
        assert!(!adjustable);
        // Its first report replaces it, whatever its revision.
        let mut theirs = state(0.8);
        theirs.revision = 0;
        rig.link.update(cx, |_, cx| {
            cx.emit(LinkEvent::Server(Event::Playback(Box::new(
                PlaybackEvent {
                    player: Some(LIVING_ROOM),
                    state: theirs,
                },
            ))));
        });
        assert_eq!(shown(cx), Some((Some(ENTRY), 0.8, false)));
        // Taking a place over with nothing moved shows nothing until it is read.
        rig.player
            .update(cx, |player, cx| player.retarget(Some(STUDIO), cx));
        assert_eq!(shown(cx), None);
    }

    fn titled(rig: &Rig, cx: &mut TestAppContext) -> Option<String> {
        rig.player.read_with(cx, |player, _| {
            player
                .state
                .as_ref()
                .and_then(|state| state.track.as_ref())
                .and_then(|track| track.title.clone())
        })
    }

    fn named_track() -> ritornel_contract::TrackSummary {
        ritornel_contract::TrackSummary {
            appearance_id: Id::from_bytes([8; 16]),
            recording_id: None,
            release_id: None,
            title: Some("Nightjar".to_owned()),
            artist: None,
            album: None,
            disc_number: None,
            track_number: None,
            duration_ms: Some(200_000),
            art_id: None,
            rating: None,
            loved: false,
        }
    }

    #[gpui::test]
    fn a_move_keeps_the_song_while_the_place_left_empties_and_the_new_one_names_it(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, None);
        let (net, _) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        rig.player.update(cx, |player, _| {
            if let Some(state) = &mut player.state {
                state.track = Some(named_track());
            }
        });
        rig.places
            .update(cx, |places, cx| places.choose(LIVING_ROOM, cx));
        // The place left says it is empty before the move is answered.
        let mut emptied = state(0.4);
        emptied.revision = 99;
        emptied.entry_id = None;
        emptied.status = PlaybackStatus::Stopped;
        rig.link.update(cx, |_, cx| {
            cx.emit(LinkEvent::Server(Event::Playback(Box::new(
                PlaybackEvent {
                    player: None,
                    state: emptied,
                },
            ))));
        });
        assert_eq!(titled(&rig, cx).as_deref(), Some("Nightjar"));
        // The living room's first report names the entry but not the song
        // yet: the song shown stays, and so does its cover.
        until(cx, "heard from the living room", |cx| {
            target(&rig, cx) == Some(LIVING_ROOM)
                && rig.player.read_with(cx, |player, _| !player.stale())
        });
        assert_eq!(titled(&rig, cx).as_deref(), Some("Nightjar"));
    }

    #[gpui::test]
    fn a_move_that_fails_shows_the_place_left_as_it_now_is(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let refusal = RpcError::new(ErrorCode::Invalid, "the entry moved on");
        let answers: Answers = Arc::new(Mutex::new(VecDeque::from([(
            "playback.move".to_owned(),
            Err(refusal),
        )])));
        let (net, _) = hub(places(), answers);
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        rig.places
            .update(cx, |places, cx| places.choose(LIVING_ROOM, cx));
        let mut emptied = state(0.4);
        emptied.revision = 99;
        emptied.entry_id = None;
        emptied.status = PlaybackStatus::Stopped;
        rig.link.update(cx, |_, cx| {
            cx.emit(LinkEvent::Server(Event::Playback(Box::new(
                PlaybackEvent {
                    player: None,
                    state: emptied,
                },
            ))));
        });
        until(cx, "heard the move fail", |cx| {
            rig.places
                .read_with(cx, |places, _| places.moving.is_none())
        });
        let entry = rig.player.read_with(cx, |player, _| {
            player.state.as_ref().map(|state| state.entry_id)
        });
        assert_eq!(entry, Some(None));
        assert_eq!(target(&rig, cx), None);
    }

    #[gpui::test]
    fn a_read_that_fails_after_a_move_is_asked_again_until_the_place_answers(
        cx: &mut TestAppContext,
    ) {
        let rig = rig(cx, None);
        let answers = Answers::default();
        let (net, asked) = hub(places(), Arc::clone(&answers));
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        // The living room is busy for its first two reads.
        {
            let busy = || {
                Err(RpcError::new(ErrorCode::Unavailable, "busy")
                    .with_detail("reason", "player_busy"))
            };
            let mut planned = answers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            planned.push_back(("playback.get".to_owned(), busy()));
            planned.push_back(("playback.get".to_owned(), busy()));
        }
        let reads = asked.of("playback.get").len();
        rig.player
            .update(cx, |player, cx| player.moved_to(Some(LIVING_ROOM), cx));
        until(cx, "asked the living room twice", |_| {
            asked.of("playback.get").len() >= reads + 2
        });
        assert!(rig.player.read_with(cx, |player, _| player.stale()));
        // A while on it is asked again, and answers.
        until(cx, "heard from the living room", |cx| {
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(2));
            rig.player.read_with(cx, |player, _| !player.stale())
        });
        assert!(rig.places.read_with(cx, |places, cx| places.adjustable(cx)));
    }

    #[gpui::test]
    fn the_place_lefts_speaker_trouble_is_not_shown_for_the_new_place(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, _) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        rig.player.update(cx, |player, _| {
            if let Some(state) = &mut player.state {
                state.output.problem = Some(OutputProblem {
                    since: 1_794_776_400_000,
                    detail: "no device is there".to_owned(),
                });
            }
        });
        rig.player
            .update(cx, |player, cx| player.moved_to(Some(LIVING_ROOM), cx));
        let look = rig.places.read_with(cx, |places, cx| places.look(cx));
        assert!(
            matches!(
                look,
                Look::There {
                    trouble: None,
                    slider: Some(_),
                    ..
                }
            ),
            "{look:?}"
        );
    }

    #[gpui::test]
    fn the_volume_icon_mutes_and_a_drag_unmutes(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, asked) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        cx.update(|cx| crate::remember::set_volume_control(VolumeControl::Always, cx));
        rig.player
            .update(cx, |player, cx| player.set_muted(true, cx));
        let look = rig.places.read_with(cx, |places, cx| places.look(cx));
        assert_eq!(
            look,
            Look::Here {
                icon: true,
                slider: Some(0.)
            }
        );
        rig.player
            .update(cx, |player, cx| player.set_volume(0.6, cx));
        until(cx, "sent both", |_| asked.of("playback.set").len() == 2);
        let sets = asked.of("playback.set");
        // Muting leaves the volume as it is.
        assert_eq!(sets[0]["muted"], json!(true));
        assert!(sets[0].get("volume").is_none());
        assert_eq!(
            (sets[1]["muted"].clone(), sets[1]["volume"].clone()),
            (json!(false), json!(0.6))
        );
    }

    #[gpui::test]
    fn a_volume_control_chosen_in_settings_shows_in_the_bar_at_once(cx: &mut TestAppContext) {
        let rig = rig(cx, None);
        let (net, _) = hub(places(), Answers::default());
        connect(&rig, net, cx);
        until(cx, "read the places and the player", |cx| listed(&rig, cx));
        let notified = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = std::rc::Rc::clone(&notified);
        let _watch =
            cx.update(|cx| cx.observe(&rig.places, move |_, _| counted.set(counted.get() + 1)));
        let look = |cx: &mut TestAppContext| rig.places.read_with(cx, |places, cx| places.look(cx));
        // The default, `remote`: this computer has the icon alone.
        assert_eq!(
            look(cx),
            Look::Here {
                icon: true,
                slider: None
            }
        );
        cx.update(|cx| crate::remember::set_volume_control(VolumeControl::Always, cx));
        cx.run_until_parked();
        assert!(notified.get() > 0, "the bar was not told");
        assert_eq!(
            look(cx),
            Look::Here {
                icon: true,
                slider: Some(0.4)
            }
        );
        // Under `never` another place shows its name alone.
        cx.update(|cx| crate::remember::set_volume_control(VolumeControl::Never, cx));
        rig.player
            .update(cx, |player, cx| player.retarget(Some(STUDIO), cx));
        until(cx, "read the studio", |cx| {
            rig.player.read_with(cx, |player, _| player.state.is_some())
        });
        assert!(matches!(look(cx), Look::There { slider: None, .. }));
        assert_eq!(
            VolumeControl::ALL.map(|each| VolumeControl::named(each.name())),
            VolumeControl::ALL
        );
    }

    #[gpui::test]
    fn without_the_players_capability_there_is_nowhere_else(cx: &mut TestAppContext) {
        let rig = rig(cx, Some(LIVING_ROOM));
        let (mut net, asked) = hub(places(), Answers::default());
        net.players = false;
        connect(&rig, net, cx);
        cx.run_until_parked();
        assert!(asked.of("players.list").is_empty());
        let (available, look) = rig
            .places
            .read_with(cx, |places, cx| (places.available(), places.look(cx)));
        assert!(!available);
        assert!(matches!(look, Look::Empty | Look::Here { icon: false, .. }));
        assert_eq!(target(&rig, cx), None);
    }
}
