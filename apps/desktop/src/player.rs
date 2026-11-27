//! The server's player as the window shows it: what is playing and where,
//! what is up next, the calls that change them, optimistic star ratings,
//! and transient error notices.
//!
//! The player followed is this server's own, or another place chosen in the
//! bar's picker (`places.rs`): then every call names it as `player`, its
//! events are followed on their own subscription, and the server's own are
//! let pass.
//!
//! The state comes whole on the `playback` topic and the position on
//! `progress`, carried on between ticks. Every command carries a key of its
//! own, so sending it again is safe ("Commands, playback, and jobs" in
//! docs/internals/contract.md). A rating set here shows at once wherever
//! the song is; if the server refuses it, it goes back.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Context, Entity, SharedString, Subscription, Task};
use ritornel_client::{Error, idempotency_key};
use ritornel_contract::methods::{
    AlbumGet, EventsSubscribe, EventsUnsubscribe, ListClose, ListOpen, Method, PlaybackGet,
    PlaybackJump, PlaybackPause, PlaybackPlay, PlaybackResume, PlaybackSeek, PlaybackSet,
    PlaybackSkip, QueueAdd, QueueMove, QueueRemove, QueueWindow, RatingsSet,
};
use ritornel_contract::{
    AlbumGetParams, AlbumRef, ContextKind, ErrorCode, FILTER_VERSION, Filter, Id, ListKind,
    ListOpenParams, PlayContext, PlaySource, PlaybackCommandParams, PlaybackGetParams,
    PlaybackJumpParams, PlaybackPlayParams, PlaybackSeekParams, PlaybackSetParams,
    PlaybackSkipParams, PlaybackState, PlaybackStatus, Predicate, ProgressEvent, Query,
    QueueAddParams, QueueEntry, QueueMoveParams, QueueOrder, QueuePlace, QueueRemoveParams,
    QueueWindowParams, RatingSetParams, Repeat, SkipDirection, Sort, SortKey, Topic, TopicsParams,
    TrackSummary, ViewParams,
};
use tokio::sync::{mpsc, oneshot, watch};

use crate::link::{Link, LinkEvent};
use crate::net::Net;

/// How long a transient notice stays visible, in milliseconds.
pub(crate) const TOAST_MS: u64 = 2600;

/// How many entries of Up next the card shows.
pub(crate) const UP_NEXT: u32 = 150;

/// What plays after the current entry, as the card last read it.
#[derive(Debug, Clone)]
pub(crate) struct UpNext {
    pub revision: u64,
    pub ratings_revision: u64,
    /// How many there are in all.
    pub total: u64,
    pub entries: Vec<QueueEntry>,
}

/// Where the playing entry was at a tick, and when.
#[derive(Debug, Clone, Copy)]
struct Tick {
    position_ms: u64,
    at: Instant,
    playing: bool,
}

/// Where a track is `elapsed` after a tick put it at `position_ms`, not past its end.
pub(crate) fn carried(position_ms: u64, elapsed: Duration, duration_ms: Option<u64>) -> u64 {
    let at = position_ms.saturating_add(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
    duration_ms.map_or(at, |end| at.min(end))
}

/// How long a player may wait for its audio before the bar shows it: a song
/// the place already has never flickers.
pub(crate) const BUFFER_QUIET: Duration = Duration::from_millis(500);
/// How long it waits before the bar says where the audio is coming from.
pub(crate) const BUFFER_SAY: Duration = Duration::from_secs(5);
/// One slow breath of the seek bar's empty part while it waits, in seconds.
const BREATH: f32 = 1.6;

/// How far into a breath the seek bar's empty part is, 0 to 1, after the
/// player has waited `waited` for its audio: nothing at first, then up and
/// down from nothing, so it starts without a jump.
pub(crate) fn breath(waited: Duration) -> f32 {
    let Some(breathing) = waited.checked_sub(BUFFER_QUIET) else {
        return 0.;
    };
    let turn = breathing.as_secs_f32() / BREATH * std::f32::consts::TAU;
    0.5 - 0.5 * turn.cos()
}

/// What the line under the title says once the wait has gone on: where the
/// audio is coming from.
pub(crate) fn getting_from(hub: Option<&str>) -> String {
    match hub.map(str::trim).filter(|hub| !hub.is_empty()) {
        Some(hub) => format!("Getting it from {hub}…"),
        None => "Getting it from the hub…".to_owned(),
    }
}

/// Since when the player has waited for its audio, given whether it was
/// waiting and is waiting now: a wait that goes on keeps its start, even
/// across songs, so the bar does not start over.
fn waiting_since(was: bool, is: bool, since: Option<Instant>, now: Instant) -> Option<Instant> {
    match (was, is) {
        (_, false) => None,
        (true, true) => since.or(Some(now)),
        (false, true) => Some(now),
    }
}

/// What the repeat button turns repeat to: off, all, one, and off again.
pub(crate) fn next_repeat(repeat: Repeat) -> Repeat {
    match repeat {
        Repeat::Off => Repeat::All,
        Repeat::All => Repeat::One,
        Repeat::One => Repeat::Off,
    }
}

/// Whole stars from a rating of 0 to 100. A love with no stars shows five.
pub(crate) fn stars_of(rating: Option<u32>, loved: bool) -> u32 {
    match rating {
        Some(rating) => (rating + 10) / 20,
        None if loved => 5,
        None => 0,
    }
}

/// What a click on star `star` sets: that many stars, or none when it is the
/// last star lit by the song's own (`stars_of`).
pub(crate) fn clicked(star: u32, own: Option<u32>) -> Option<u32> {
    (own.map(|own| stars_of(Some(own), false)) != Some(star)).then_some(star * 20)
}

/// What the card shows once `moving` has gone to `place`: at the top,
/// straight after another, or at the end, which is past what the card holds
/// when it has not read the whole of Up next (`complete`).
pub(crate) fn moved(
    entries: &[QueueEntry],
    moving: &[Id],
    place: QueuePlace,
    complete: bool,
) -> Vec<QueueEntry> {
    let (taken, kept): (Vec<QueueEntry>, Vec<QueueEntry>) = entries
        .iter()
        .cloned()
        .partition(|entry| moving.contains(&entry.entry_id));
    let at = match place {
        QueuePlace::End => {
            return if complete {
                kept.into_iter().chain(taken).collect()
            } else {
                kept
            };
        }
        QueuePlace::Next => 0,
        QueuePlace::After { entry_id } => kept
            .iter()
            .position(|entry| entry.entry_id == entry_id)
            .map_or(0, |at| at + 1),
    };
    let (before, after) = kept.split_at(at);
    before.iter().chain(&taken).chain(after).cloned().collect()
}

/// Where entries of Up next dropped at `place` go: at the top, straight
/// after another, or to the end of all of Up next, however much of it the
/// card has read. `None` for entries dropped on themselves.
pub(crate) fn move_to(place: QueuePlace, moving: &[Id]) -> Option<QueuePlace> {
    match place {
        QueuePlace::After { entry_id } if moving.contains(&entry_id) => None,
        _ => Some(place),
    }
}

/// What a call that failed says, in the window's words. `None` for a
/// conflict, which says nothing: Up next is read again instead.
fn said(error: &Error) -> Option<&'static str> {
    Some(match error {
        // Another place's trouble is said briefly: the bar names the place.
        Error::Call(rpc) if reason(rpc) == Some("player_unreachable") => "Out of reach right now",
        Error::Call(rpc) if reason(rpc) == Some("player_busy") => "Busy. Try again in a moment",
        Error::Call(rpc) => match rpc.data.code {
            ErrorCode::Unsupported => "There’s no player there",
            ErrorCode::Unavailable => "The server cannot play that now",
            ErrorCode::NotFound => "That is no longer in the library",
            ErrorCode::Conflict => return None,
            _ => "That did not work",
        },
        Error::Transport(_) => "Not connected to the server",
        _ => "That did not work",
    })
}

/// The `reason` an error gives, if any.
pub(crate) fn reason(rpc: &ritornel_contract::RpcError) -> Option<&str> {
    rpc.data
        .details
        .get("reason")
        .and_then(serde_json::Value::as_str)
}

/// How long a volume set here is shown over the place's own reports of
/// it, until one says the same: those sent before it arrive late.
const VOLUME_SETTLES: Duration = Duration::from_millis(1500);

/// The sound set here last, and when: a volume, or none for muting alone.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SoundSet {
    /// Counts the sets, so that one is told from a later one alike.
    id: u64,
    volume: Option<f64>,
    muted: bool,
    /// When it was answered, or sent while it is not yet.
    at: Instant,
}

/// What the player's state says of its sound, given what was set here
/// last and how many sets are still unanswered: while any is, and for a
/// while after the last is answered, a report of another sound is one sent
/// before it, and what was set stands. Whether the set is still waited for.
fn settled(
    state: &mut PlaybackState,
    set: Option<SoundSet>,
    unanswered: usize,
    now: Instant,
) -> bool {
    let Some(set) = set else {
        return false;
    };
    let same = set
        .volume
        .is_none_or(|volume| (state.volume - volume).abs() < 1e-6)
        && state.muted == set.muted;
    if unanswered == 0 && (same || now.duration_since(set.at) >= VOLUME_SETTLES) {
        return false;
    }
    if let Some(volume) = set.volume {
        state.volume = volume;
    }
    state.muted = set.muted;
    true
}

/// The topics followed for another place.
const PLACE_TOPICS: [Topic; 3] = [Topic::Playback, Topic::Progress, Topic::Queue];

/// How long to wait before asking again to follow a place's events.
const FOLLOW_AGAIN: Duration = Duration::from_secs(2);

/// Pending clicks take precedence over a receipt, then the stars in the row.
#[derive(Default)]
struct Ratings {
    pending: HashMap<Id, (Id, Option<u32>)>,
    confirmed: HashMap<Id, (u64, Option<u32>)>,
    revision: u64,
    version: u64,
}

impl Ratings {
    fn start(&mut self, recording: Id, key: Id, rating: Option<u32>) {
        self.pending.insert(recording, (key, rating));
        self.version += 1;
    }

    fn own(&self, recording: Id, saved: Option<u32>, row_revision: Option<u64>) -> Option<u32> {
        if let Some((_, rating)) = self.pending.get(&recording) {
            return *rating;
        }
        if let Some(&(written, rating)) = self.confirmed.get(&recording) {
            // Playback events have their own clock. List rows carry a library revision.
            if row_revision.map_or(written >= self.revision, |read| written > read) {
                return rating;
            }
        }
        saved
    }

    fn finish(&mut self, recording: Id, key: Id) -> bool {
        if self
            .pending
            .get(&recording)
            .is_some_and(|(pending, _)| *pending == key)
        {
            self.pending.remove(&recording);
            self.version += 1;
            true
        } else {
            false
        }
    }

    /// The receipt can arrive on either side of its own library event.
    fn confirm(&mut self, recording: Id, rating: Option<u32>, revision: u64) {
        self.revision = self.revision.max(revision);
        if self
            .confirmed
            .get(&recording)
            .is_none_or(|(kept, _)| revision > *kept)
        {
            self.confirmed.insert(recording, (revision, rating));
            self.version += 1;
        }
    }
}

/// Takes a state unless it is no newer than the one held.
fn keep_state(
    kept: &mut Option<PlaybackState>,
    state: PlaybackState,
) -> Option<&mut PlaybackState> {
    if kept
        .as_ref()
        .is_some_and(|previous| state.revision <= previous.revision)
    {
        return None;
    }
    Some(kept.insert(state))
}

/// Keeps a session following the events of the place `wanted` names, and
/// of no other, telling `followed` each place once it is followed. It ends,
/// letting the last place go, when `wanted`'s sender is dropped.
async fn follow_places(
    client: Arc<ritornel_client::Client>,
    mut wanted: watch::Receiver<Option<Id>>,
    followed: mpsc::UnboundedSender<Id>,
) {
    let topics = |player| TopicsParams {
        player: Some(player),
        topics: PLACE_TOPICS.to_vec(),
    };
    let mut held: Option<Id> = None;
    loop {
        let want = *wanted.borrow_and_update();
        if want != held {
            if let Some(old) = held.take() {
                let _ = client.call::<EventsUnsubscribe>(&topics(old)).await;
            }
            if let Some(new) = want {
                if client.call::<EventsSubscribe>(&topics(new)).await.is_ok() {
                    held = Some(new);
                    let _ = followed.send(new);
                } else {
                    tokio::select! {
                        () = tokio::time::sleep(FOLLOW_AGAIN) => {}
                        changed = wanted.changed() => if changed.is_err() { return },
                    }
                }
            }
            continue;
        }
        if wanted.changed().await.is_err() {
            if let Some(old) = held {
                let _ = client.call::<EventsUnsubscribe>(&topics(old)).await;
            }
            return;
        }
    }
}

pub(crate) struct Player {
    net: Option<Arc<Net>>,
    /// The place played on, from `players.list`, or `None` for this
    /// server's own player.
    target: Option<Id>,
    /// Counts changes of place and session: an answer for an earlier one
    /// is dropped.
    generation: u64,
    /// The place whose events this session follows (`follow`), and what
    /// reads its state once it is followed.
    following: Option<(watch::Sender<Option<Id>>, Task<()>)>,
    /// The sound last set here, until the place says the same.
    volume_set: Option<SoundSet>,
    /// Whether the state shown is the place left's, kept through a move
    /// until the new place's first report replaces it.
    stale: bool,
    /// The song shown through a move, by its entry: the new place's first
    /// reports of that entry may not name it yet.
    kept_track: Option<(Id, TrackSummary, Option<u64>)>,
    /// Whether a move is under way: the place it leaves may say it is
    /// empty before the move is answered, and that is held back here, to
    /// be shown only if the move fails.
    moving: bool,
    held_back: Option<PlaybackState>,
    /// Sound sets not yet answered.
    sounds_unanswered: usize,
    /// Counts the sound sets (`SoundSet::id`).
    sound_sets: u64,
    /// Reads the state again after a read failed while the song kept
    /// through a move waits on it.
    read_again_later: Option<Task<()>>,
    /// Ends when the last volume sent has been answered: the next waits
    /// for it, so the place hears them in order.
    volume_sent: Option<oneshot::Receiver<()>>,
    /// Reads the state again if the volume set is never confirmed.
    volume_check: Option<Task<()>>,
    /// Whether the place is out of reach: the clock holds.
    pub out_of_reach: bool,
    /// The player's state, whole, or `None` before it is read or on a server with none.
    pub state: Option<PlaybackState>,
    tick: Tick,
    /// Moves the position on while playing, four times a second.
    ticker: Option<Task<()>>,
    /// How long the bars of the song playing have moved, and since when
    /// they have been moving: they hold while it is paused.
    eq_base: Duration,
    eq_since: Option<Instant>,
    rated: Ratings,
    /// What is being said, and since when.
    pub toast: Option<(SharedString, Instant)>,
    toast_ends: Option<Task<()>>,
    /// The time the hub went, if the toast has said it is away and not yet
    /// that it is back. Kept across reconnecting to this app's own server,
    /// so one absence is said once and its end is not missed.
    hub_away_said: Option<i64>,
    /// What plays next, while the card is open.
    pub up_next: Option<UpNext>,
    /// Monotonic counter bumped whenever `up_next` changes (server read or
    /// optimistic reorder/removal) so observers see queue edits immediately.
    up_next_version: u64,
    /// Whether Up next's card is open.
    pub open: bool,
    /// The last read of Up next asked for: an older one that answers late is dropped.
    reading: u64,
    /// Since when the player has waited for its audio (`Buffering`), and
    /// what wakes the bar when the wait is long enough to show and to say.
    buffering_since: Option<Instant>,
    buffering_wakes: Option<Task<()>>,
    _link: Subscription,
}

impl Player {
    pub(crate) fn new(link: &Entity<Link>, cx: &mut Context<Self>) -> Self {
        let followed = cx.subscribe(link, |player, _, event: &LinkEvent, cx| match event {
            LinkEvent::Connected(net) => {
                player.state = None;
                player.end_wait();
                player.stale = false;
                player.sounds_unanswered = 0;
                player.rated = Ratings::default();
                player.generation += 1;
                player.volume_set = None;
                player.volume_sent = None;
                player.net = Some(Arc::clone(net));
                player.say_of_hub(net, cx);
                // A new session follows nothing of another place's yet.
                player.follow(cx);
                player.read(cx);
            }
            // The same session: nothing it read has changed, but what it
            // says of its hub may have.
            LinkEvent::Restated(net) => {
                player.net = Some(Arc::clone(net));
                player.say_of_hub(net, cx);
            }
            LinkEvent::Away => {
                player.net = None;
                player.generation += 1;
                player.following = None;
                player.volume_sent = None;
                player.rated = Ratings::default();
                player.state = None;
                player.end_wait();
                player.stale = false;
                player.sounds_unanswered = 0;
                player.tick.playing = false;
                player.carry(cx);
                cx.notify();
            }
            // Only the place followed: the others' events pass.
            LinkEvent::Server(event)
                if event.topic().is_per_player() && event.player() != player.target => {}
            LinkEvent::Server(ritornel_contract::Event::Playback(event)) => {
                player.took(event.state.clone(), cx);
            }
            LinkEvent::Server(ritornel_contract::Event::Progress(event)) => {
                player.ticked(event, cx);
            }
            LinkEvent::Server(ritornel_contract::Event::Library(event))
                if event.changed.is_empty()
                    || event
                        .changed
                        .contains(&ritornel_contract::LibraryChange::Ratings) =>
            {
                player.rated.revision = player.rated.revision.max(event.library_revision);
                if player.open {
                    player.read_up_next(cx);
                }
                cx.notify();
            }
            LinkEvent::Server(ritornel_contract::Event::Queue(_)) => {
                if player.open {
                    player.read_up_next(cx);
                }
            }
            LinkEvent::Server(_) => {}
        });
        let mut player = Self {
            net: None,
            target: None,
            generation: 0,
            following: None,
            volume_set: None,
            stale: false,
            kept_track: None,
            moving: false,
            held_back: None,
            sounds_unanswered: 0,
            sound_sets: 0,
            read_again_later: None,
            volume_sent: None,
            volume_check: None,
            out_of_reach: false,
            state: None,
            tick: Tick {
                position_ms: 0,
                at: Instant::now(),
                playing: false,
            },
            ticker: None,
            eq_base: Duration::ZERO,
            eq_since: None,
            rated: Ratings::default(),
            toast: None,
            hub_away_said: None,
            toast_ends: None,
            up_next: None,
            up_next_version: 0,
            open: false,
            reading: 0,
            buffering_since: None,
            buffering_wakes: None,
            _link: followed,
        };
        if let Some(net) = link.read(cx).net() {
            player.say_of_hub(&net, cx);
            player.net = Some(net);
            player.read(cx);
        }
        player
    }

    /// Whether something is playing now, or about to once its master is
    /// here: the toggle pauses it either way.
    pub(crate) fn playing(&self) -> bool {
        self.state.as_ref().is_some_and(|state| {
            matches!(
                state.status,
                PlaybackStatus::Playing | PlaybackStatus::Buffering
            )
        })
    }

    /// How long the player has waited for its audio, while it waits.
    pub(crate) fn buffering(&self) -> Option<Duration> {
        let waiting = self
            .state
            .as_ref()
            .is_some_and(|state| state.status == PlaybackStatus::Buffering);
        waiting
            .then_some(self.buffering_since)
            .flatten()
            .map(|since| since.elapsed())
    }

    /// The hub's name, on a satellite.
    pub(crate) fn hub_name(&self) -> Option<&str> {
        Some(self.net.as_ref()?.hub.as_ref()?.name.as_str())
    }

    /// Whether the player is on an entry: the transport works.
    pub(crate) fn on(&self) -> bool {
        self.state
            .as_ref()
            .is_some_and(|state| state.entry_id.is_some())
    }

    /// The appearance playing, to mark its row wherever it is listed.
    pub(crate) fn appearance(&self) -> Option<Id> {
        self.state
            .as_ref()?
            .track
            .as_ref()
            .map(|track| track.appearance_id)
    }

    /// Where the playing entry is now, carried on between ticks, and held
    /// while the place is out of reach.
    pub(crate) fn position(&self) -> u64 {
        let duration = self.state.as_ref().and_then(|state| state.duration_ms);
        if self.advancing() && !self.out_of_reach {
            carried(self.tick.position_ms, self.tick.at.elapsed(), duration)
        } else {
            self.tick.position_ms
        }
    }

    fn read(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.net.clone() else { return };
        let client = Arc::clone(&net.client);
        let target = self.target;
        let generation = self.generation;
        let read = net.rt().spawn(async move {
            client
                .call::<PlaybackGet>(&PlaybackGetParams { player: target })
                .await
        });
        cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |player, cx| {
                // A place left meanwhile, or a session ended, says nothing.
                if player.generation != generation {
                    return;
                }
                match answer {
                    Ok(Ok(state)) => player.took(state, cx),
                    // The song kept through a move waits on this read, and a
                    // place that is paused says nothing more by itself: ask again.
                    _ if player.stale => {
                        player.read_again_later = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(FOLLOW_AGAIN).await;
                            this.update(cx, |player, cx| {
                                if player.generation == generation && player.stale {
                                    player.read(cx);
                                }
                            })
                            .ok();
                        }));
                    }
                    _ => {}
                }
                if player.open {
                    player.read_up_next(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Takes a state, unless it is older than the one held.
    fn took(&mut self, state: PlaybackState, cx: &mut Context<Self>) {
        crate::debug(format_args!(
            "playback: revision {}, {:?}, entry {:?} at {} ms, queue {}",
            state.revision, state.status, state.entry_id, state.position_ms, state.queue_length
        ));
        // The place a move leaves may say it is empty before the move is
        // answered: that waits, to be shown if the move fails.
        if self.moving && state.entry_id.is_none() {
            self.held_back = Some(state);
            return;
        }
        let before = self.state.as_ref().and_then(|kept| kept.entry_id);
        let was_buffering = self
            .state
            .as_ref()
            .is_some_and(|kept| kept.status == PlaybackStatus::Buffering);
        // The place left's state gives way to the new place's first, whatever its revision.
        if std::mem::take(&mut self.stale) {
            self.state = None;
        }
        let Some(state) = keep_state(&mut self.state, state) else {
            return;
        };
        // The song moved keeps its name and cover until the place names it.
        match &self.kept_track {
            Some((entry, track, duration))
                if state.entry_id == Some(*entry) && state.track.is_none() =>
            {
                state.track = Some(track.clone());
                state.duration_ms = state.duration_ms.or(*duration);
            }
            Some(_) => self.kept_track = None,
            None => {}
        }
        if !settled(
            state,
            self.volume_set,
            self.sounds_unanswered,
            Instant::now(),
        ) {
            self.volume_set = None;
        }
        let entry = state.entry_id;
        let buffering = state.status == PlaybackStatus::Buffering;
        self.tick = Tick {
            position_ms: state.position_ms,
            at: Instant::now(),
            playing: state.status == PlaybackStatus::Playing,
        };
        self.wait_for_audio(was_buffering, buffering, cx);
        self.carry(cx);
        // What is next moves on with the entry.
        if self.open && before != entry {
            self.read_up_next(cx);
        }
        cx.notify();
    }

    fn ticked(&mut self, event: &ProgressEvent, cx: &mut Context<Self>) {
        crate::debug(format_args!(
            "progress: {} at {} ms, playing {}",
            event.entry_id, event.position_ms, event.playing
        ));
        // The song kept through a move holds still until the place reports it whole.
        if self.stale
            || Some(event.entry_id) != self.state.as_ref().and_then(|state| state.entry_id)
        {
            return;
        }
        self.tick = Tick {
            position_ms: event.position_ms,
            at: Instant::now(),
            playing: event.playing,
        };
        // Sound has come, though the state that says so may not have yet:
        // the two topics are sent apart.
        if event.playing {
            self.end_wait();
        }
        self.carry(cx);
        cx.notify();
    }

    /// Follows a wait for audio: the bar is woken when it has gone on long
    /// enough to show and again to say, and is shown no more once it ends.
    fn wait_for_audio(&mut self, was: bool, is: bool, cx: &mut Context<Self>) {
        let since = waiting_since(was, is, self.buffering_since, Instant::now());
        if since.is_some() && self.buffering_since.is_none() {
            self.buffering_wakes = Some(cx.spawn(async move |this, cx| {
                for wait in [BUFFER_QUIET, BUFFER_SAY.saturating_sub(BUFFER_QUIET)] {
                    cx.background_executor().timer(wait).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
        if since.is_none() {
            self.buffering_wakes = None;
        }
        self.buffering_since = since;
    }

    /// Ends a wait for audio and what would wake the bar for it.
    fn end_wait(&mut self) {
        self.buffering_since = None;
        self.buffering_wakes = None;
    }

    /// Whether the playback clock is advancing between progress reports.
    pub(crate) fn advancing(&self) -> bool {
        self.tick.playing
    }

    pub(crate) fn equalizer(&self, cx: &gpui::App) -> crate::marks::Equalizer {
        crate::marks::Equalizer::playback(self.eq_base, self.eq_since, cx)
    }

    /// Moves the position on while playing, a few times a second.
    fn carry(&mut self, cx: &mut Context<Self>) {
        match (self.tick.playing, self.eq_since) {
            (true, None) => self.eq_since = Some(Instant::now()),
            (false, Some(since)) => {
                self.eq_base += since.elapsed();
                self.eq_since = None;
            }
            _ => {}
        }
        if !self.tick.playing {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    /// Says once that the hub has gone, when it has been away long enough
    /// to say so (`crate::away`), and once that it is back.
    fn say_of_hub(&mut self, net: &Net, cx: &mut Context<Self>) {
        let (word, said) =
            crate::away::notice(self.hub_away_said, net.hub.as_ref(), crate::away::now_ms());
        self.hub_away_said = said;
        if let Some(word) = word {
            self.say(word, cx);
        }
    }

    /// Says `text` for a moment, in place of whatever was being said.
    pub(crate) fn say(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.toast = Some((text.into(), Instant::now()));
        self.toast_ends = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(TOAST_MS))
                .await;
            this.update(cx, |player, cx| {
                player.toast = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Sends a call, and says so if it fails. `None` when it did not work.
    fn send<M>(&mut self, params: M::Params, cx: &mut Context<Self>) -> Task<Option<M::Result>>
    where
        M: Method + 'static,
        M::Params: Send + Sync + 'static,
        M::Result: Send + 'static,
    {
        let Some(net) = self.net.clone() else {
            self.say("Not connected to the server", cx);
            return Task::ready(None);
        };
        let client = Arc::clone(&net.client);
        let generation = self.generation;
        let call = net
            .rt()
            .spawn(async move { client.call::<M>(&params).await });
        cx.spawn(async move |this, cx| match call.await {
            Ok(Ok(result)) => Some(result),
            Ok(Err(error)) => {
                this.update(cx, |player, cx| {
                    // The place it was sent to has been left: nothing is said over another.
                    if player.generation != generation {
                        return;
                    }
                    match said(&error) {
                        Some(words) => player.say(words, cx),
                        // Up next changed under the edit: what it is now is read again.
                        None => player.read_up_next(cx),
                    }
                })
                .ok();
                None
            }
            Err(_) => None,
        })
    }

    /// A new command's key, or a word that there is none.
    fn key(&mut self, cx: &mut Context<Self>) -> Option<Id> {
        let key = idempotency_key().ok();
        if key.is_none() {
            self.say("That did not work", cx);
        }
        key
    }

    /// Plays `source` in place of the queue. Whether the server took it.
    pub(crate) fn play(
        &mut self,
        source: PlaySource,
        start: Option<u32>,
        context: Option<PlayContext>,
        shuffle: bool,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let Some(key) = self.key(cx) else {
            return Task::ready(false);
        };
        let sent = self.send::<PlaybackPlay>(
            PlaybackPlayParams {
                player: self.target,
                idempotency_key: key,
                source,
                start,
                context,
                shuffle: shuffle.then_some(true),
            },
            cx,
        );
        cx.spawn(async move |_, _| sent.await.is_some())
    }

    /// Plays, or pauses.
    pub(crate) fn toggle(&mut self, cx: &mut Context<Self>) {
        if !self.on() {
            return;
        }
        let Some(key) = self.key(cx) else { return };
        let params = PlaybackCommandParams {
            player: self.target,
            idempotency_key: key,
        };
        if self.playing() {
            self.send::<PlaybackPause>(params, cx).detach();
        } else {
            self.send::<PlaybackResume>(params, cx).detach();
        }
    }

    pub(crate) fn skip(&mut self, direction: SkipDirection, cx: &mut Context<Self>) {
        let Some(from) = self.state.as_ref().and_then(|state| state.entry_id) else {
            return;
        };
        let Some(key) = self.key(cx) else { return };
        self.send::<PlaybackSkip>(
            PlaybackSkipParams {
                player: self.target,
                idempotency_key: key,
                from_entry_id: from,
                direction,
            },
            cx,
        )
        .detach();
    }

    /// Moves the playing entry to `position_ms`, and shows it there at once.
    pub(crate) fn seek(&mut self, entry: Id, position_ms: u64, cx: &mut Context<Self>) {
        if self.state.as_ref().and_then(|state| state.entry_id) != Some(entry) {
            return;
        }
        let Some(key) = self.key(cx) else { return };
        self.tick.position_ms = position_ms;
        self.tick.at = Instant::now();
        cx.notify();
        self.send::<PlaybackSeek>(
            PlaybackSeekParams {
                player: self.target,
                idempotency_key: key,
                entry_id: entry,
                position_ms,
            },
            cx,
        )
        .detach();
    }

    fn set(&mut self, shuffle: Option<bool>, repeat: Option<Repeat>, cx: &mut Context<Self>) {
        let Some(key) = self.key(cx) else { return };
        self.send::<PlaybackSet>(
            PlaybackSetParams {
                player: self.target,
                idempotency_key: key,
                volume: None,
                muted: None,
                shuffle,
                repeat,
            },
            cx,
        )
        .detach();
    }

    /// Sets the place's volume, from 0 to 1, and unmutes it.
    pub(crate) fn set_volume(&mut self, volume: f64, cx: &mut Context<Self>) {
        self.set_sound(Some(volume.clamp(0., 1.)), false, cx);
    }

    /// Mutes the place, or unmutes it, leaving its volume as it is.
    pub(crate) fn set_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        self.set_sound(None, muted, cx);
    }

    /// Sets the place's sound and shows it at once. Each set waits for the
    /// one before, so the place hears them in order. Reports of another
    /// sound that follow soon after were sent before it, and pass
    /// (`settled`); if none confirms it, or it fails, the state is read
    /// again.
    fn set_sound(&mut self, volume: Option<f64>, muted: bool, cx: &mut Context<Self>) {
        if self.stale {
            return;
        }
        let (Some(state), Some(net)) = (self.state.as_mut(), self.net.clone()) else {
            return;
        };
        let Ok(key) = idempotency_key() else {
            return;
        };
        if let Some(volume) = volume {
            state.volume = volume;
        }
        state.muted = muted;
        self.sound_sets += 1;
        let set = SoundSet {
            id: self.sound_sets,
            volume,
            muted,
            at: Instant::now(),
        };
        self.volume_set = Some(set);
        cx.notify();
        let params = PlaybackSetParams {
            player: self.target,
            idempotency_key: key,
            volume,
            muted: Some(muted),
            shuffle: None,
            repeat: None,
        };
        self.sounds_unanswered += 1;
        let client = Arc::clone(&net.client);
        let before = self.volume_sent.take();
        let (sent, answered) = oneshot::channel();
        self.volume_sent = Some(answered);
        let call = net.rt().spawn(async move {
            if let Some(before) = before {
                let _ = before.await;
            }
            let answer = client.call::<PlaybackSet>(&params).await;
            let _ = sent.send(());
            answer
        });
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let failed = !matches!(call.await, Ok(Ok(_)));
            this.update(cx, |player, cx| {
                if player.generation != generation {
                    return;
                }
                player.sounds_unanswered = player.sounds_unanswered.saturating_sub(1);
                if failed {
                    player.say("The volume could not be set", cx);
                    player.check_sound(set.id, cx);
                } else if let Some(latest) = player
                    .volume_set
                    .as_mut()
                    .filter(|latest| latest.id == set.id)
                {
                    // The last set is answered: its while begins now, and if
                    // no report confirms it by then, the state is read again.
                    latest.at = Instant::now();
                    player.volume_check = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(VOLUME_SETTLES).await;
                        this.update(cx, |player, cx| {
                            if player.generation == generation {
                                player.check_sound(set.id, cx);
                            }
                        })
                        .ok();
                    }));
                }
            })
            .ok();
        })
        .detach();
    }

    /// Reads the state again in place of the sound `set` showed, unless a
    /// later set or a report has taken its place.
    fn check_sound(&mut self, set: u64, cx: &mut Context<Self>) {
        if self.volume_set.is_none_or(|latest| latest.id != set) {
            return;
        }
        self.volume_set = None;
        self.read_sound(cx);
    }

    /// Reads the place's sound again, to put right what was shown ahead of
    /// it: a state of the revision shown gives its volume and muting alone,
    /// so the read cannot move the clock or the wait back; a newer one is
    /// taken whole.
    fn read_sound(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.net.clone() else { return };
        let client = Arc::clone(&net.client);
        let target = self.target;
        let generation = self.generation;
        let read = net.rt().spawn(async move {
            client
                .call::<PlaybackGet>(&PlaybackGetParams { player: target })
                .await
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(read)) = read.await else { return };
            this.update(cx, |player, cx| {
                if player.generation != generation || player.volume_set.is_some() {
                    return;
                }
                match &mut player.state {
                    Some(shown) if shown.revision == read.revision => {
                        shown.volume = read.volume;
                        shown.muted = read.muted;
                        cx.notify();
                    }
                    _ => player.took(read, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Reads the place's state again.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.read(cx);
    }

    /// A move begins: what the place it leaves says while it is empty is
    /// held back until the move is answered.
    pub(crate) fn move_began(&mut self) {
        self.moving = true;
    }

    /// The move failed: the place stays, as it now is.
    pub(crate) fn move_failed(&mut self, cx: &mut Context<Self>) {
        self.moving = false;
        if let Some(state) = self.held_back.take() {
            self.took(state, cx);
        }
    }

    /// Whether the state shown is the place left's, until the new place reports.
    pub(crate) fn stale(&self) -> bool {
        self.stale
    }

    /// The place played on: `None` for this server's own player.
    pub(crate) fn target(&self) -> Option<Id> {
        self.target
    }

    /// The count of changes of place and session, for what must not
    /// outlive one.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Plays on another place from here on, or on this server's own player
    /// with `None`: its events are followed in place of the last one's,
    /// and its state, read again, replaces what was shown.
    pub(crate) fn retarget(&mut self, target: Option<Id>, cx: &mut Context<Self>) {
        self.switch(target, false, cx);
    }

    /// Plays on the place the music has just moved to: what was shown
    /// stays, the clock held, until the place's first report replaces it,
    /// so the song does not blink out and back between two places.
    pub(crate) fn moved_to(&mut self, target: Option<Id>, cx: &mut Context<Self>) {
        self.switch(target, true, cx);
    }

    fn switch(&mut self, target: Option<Id>, moved: bool, cx: &mut Context<Self>) {
        if target == self.target {
            return;
        }
        crate::debug(format_args!("player: from {:?} to {target:?}", self.target));
        self.target = target;
        self.generation += 1;
        self.volume_set = None;
        self.volume_sent = None;
        self.volume_check = None;
        self.out_of_reach = false;
        let kept = moved && self.state.is_some();
        self.stale = kept;
        // A wait for the place left's audio is not the new place's.
        self.end_wait();
        self.moving = false;
        self.held_back = None;
        self.sounds_unanswered = 0;
        self.kept_track = self
            .state
            .as_ref()
            .filter(|_| kept)
            .and_then(|state| Some((state.entry_id?, state.track.clone()?, state.duration_ms)));
        self.tick = Tick {
            position_ms: if kept { self.position() } else { 0 },
            at: Instant::now(),
            playing: false,
        };
        self.carry(cx);
        if !kept {
            self.state = None;
            self.up_next = None;
            self.up_next_version = self.up_next_version.wrapping_add(1);
        }
        // A read of the place left that answers late is dropped.
        self.reading += 1;
        match &self.following {
            Some((wanted, _)) => {
                wanted.send_replace(target);
            }
            None => self.follow(cx),
        }
        self.read(cx);
        if self.open {
            self.read_up_next(cx);
        }
        cx.notify();
    }

    /// Follows the target's events on this session: one task changes what
    /// the session follows, a change at a time and in order, asking again
    /// every `FOLLOW_AGAIN` until it works, and lets the last place go when
    /// the player or the session does. Once a place is followed, its state
    /// is read again, since it may have changed before.
    fn follow(&mut self, cx: &mut Context<Self>) {
        self.following = None;
        let Some(net) = self.net.clone() else { return };
        let (wanted, asked) = watch::channel(self.target);
        let (followed, mut heard) = mpsc::unbounded_channel();
        net.rt()
            .spawn(follow_places(Arc::clone(&net.client), asked, followed));
        let reads = cx.spawn(async move |this, cx| {
            while let Some(place) = heard.recv().await {
                let done = this.update(cx, |player, cx| {
                    if player.target == Some(place) {
                        player.read(cx);
                    }
                });
                if done.is_err() {
                    return;
                }
            }
        });
        self.following = Some((wanted, reads));
    }

    pub(crate) fn set_shuffle(&mut self, shuffle: bool, cx: &mut Context<Self>) {
        self.set(Some(shuffle), None, cx);
    }

    pub(crate) fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        let repeat = next_repeat(
            self.state
                .as_ref()
                .map_or(Repeat::Off, |state| state.repeat),
        );
        self.set(None, Some(repeat), cx);
    }

    /// Plays an entry of Up next now.
    pub(crate) fn jump(&mut self, entry: Id, cx: &mut Context<Self>) {
        let Some(key) = self.key(cx) else { return };
        self.send::<PlaybackJump>(
            PlaybackJumpParams {
                player: self.target,
                idempotency_key: key,
                entry_id: entry,
            },
            cx,
        )
        .detach();
    }

    /// Adds `source` to the queue at `place`. With nothing in the queue it
    /// plays it: the server decides that, as it adds, for this window may
    /// not have heard of the queue yet. With the card closed, the window
    /// says where it went, unless it seemed to start playing.
    pub(crate) fn add(
        &mut self,
        source: PlaySource,
        place: QueuePlace,
        name: String,
        context: Option<PlayContext>,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let Some(key) = self.key(cx) else {
            return Task::ready(false);
        };
        let seemed_empty = self
            .state
            .as_ref()
            .is_none_or(|state| state.queue_length == 0);
        let sent = self.send::<QueueAdd>(
            QueueAddParams {
                player: self.target,
                idempotency_key: key,
                source,
                place,
                context,
                expected_revision: None,
                play_if_empty: Some(true),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let done = sent.await.is_some();
            if done && !seemed_empty {
                let words = if matches!(place, QueuePlace::Next) {
                    format!("{name} plays next")
                } else {
                    format!("{name} is in Up next")
                };
                this.update(cx, |player, cx| {
                    if !player.open {
                        player.say(words, cx);
                    }
                })
                .ok();
            }
            done
        })
    }

    /// Monotonic version of `up_next` (server updates and optimistic edits).
    pub(crate) fn up_next_version(&self) -> u64 {
        self.up_next_version
    }

    /// Takes an entry out of Up next, and out of the card at once.
    pub(crate) fn remove(&mut self, entry: Id, cx: &mut Context<Self>) {
        let Some(key) = self.key(cx) else { return };
        let revision = self.up_next.as_ref().map(|up_next| up_next.revision);
        if let Some(up_next) = &mut self.up_next {
            up_next.total = up_next.total.saturating_sub(1);
            up_next.entries.retain(|kept| kept.entry_id != entry);
            self.up_next_version = self.up_next_version.wrapping_add(1);
            cx.notify();
        }
        self.send::<QueueRemove>(
            QueueRemoveParams {
                player: self.target,
                idempotency_key: key,
                entry_ids: vec![entry],
                expected_revision: revision,
            },
            cx,
        )
        .detach();
    }

    /// Puts entries of Up next at its top, straight after another, or at
    /// its end, and shows them there at once.
    pub(crate) fn move_entries(
        &mut self,
        entries: Vec<Id>,
        place: QueuePlace,
        cx: &mut Context<Self>,
    ) {
        let Some(revision) = self.up_next.as_ref().map(|up_next| up_next.revision) else {
            return;
        };
        let Some(key) = self.key(cx) else { return };
        if let Some(up_next) = &mut self.up_next {
            let complete = up_next.entries.len() as u64 >= up_next.total;
            up_next.entries = moved(&up_next.entries, &entries, place, complete);
            self.up_next_version = self.up_next_version.wrapping_add(1);
            cx.notify();
        }
        self.send::<QueueMove>(
            QueueMoveParams {
                player: self.target,
                idempotency_key: key,
                entry_ids: entries,
                place,
                expected_revision: Some(revision),
            },
            cx,
        )
        .detach();
    }

    /// Reads what plays next. The card keeps what it showed until a read works.
    pub(crate) fn read_up_next(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.net.clone() else { return };
        self.reading += 1;
        let asked = self.reading;
        let ratings_revision = self.rated.revision;
        let client = Arc::clone(&net.client);
        let target = self.target;
        let read = net.rt().spawn(async move {
            client
                .call::<QueueWindow>(&QueueWindowParams {
                    player: target,
                    offset: 0,
                    limit: Some(UP_NEXT),
                    order: Some(QueueOrder::UpNext),
                })
                .await
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(window)) = read.await else { return };
            this.update(cx, |player, cx| {
                if asked != player.reading
                    || player
                        .net
                        .as_ref()
                        .is_none_or(|current| !current.same_session(&net))
                {
                    return;
                }
                crate::debug(format_args!(
                    "up next: revision {}, {} of {}",
                    window.queue_revision,
                    window.entries.len(),
                    window.total
                ));
                player.up_next = Some(UpNext {
                    revision: window.queue_revision,
                    ratings_revision,
                    total: window.total,
                    entries: window.entries,
                });
                player.up_next_version = player.up_next_version.wrapping_add(1);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Opens or closes Up next's card.
    pub(crate) fn toggle_card(&mut self, open: Option<bool>, cx: &mut Context<Self>) {
        self.open = open.unwrap_or(!self.open);
        if self.open {
            self.read_up_next(cx);
        }
        cx.notify();
    }

    /// The listener's own rating of a recording: one set here, or what the server said.
    pub(crate) fn own(
        &self,
        recording: Id,
        said: Option<u32>,
        row_revision: Option<u64>,
    ) -> Option<u32> {
        self.rated.own(recording, said, row_revision)
    }

    /// Monotonic counter bumped on optimistic or confirmed rating changes.
    pub(crate) fn ratings_version(&self) -> u64 {
        self.rated.version
    }

    /// A click on star `star` of `recording`, whose stars the server last
    /// said were `said`: that many, or none when it has exactly that many.
    pub(crate) fn click_star(
        &mut self,
        recording: Id,
        said: Option<u32>,
        row_revision: Option<u64>,
        star: u32,
        cx: &mut Context<Self>,
    ) {
        let to = clicked(star, self.own(recording, said, row_revision));
        self.rate(recording, to, cx);
    }

    /// Sets a recording's stars, or takes them away with `None`.
    pub(crate) fn rate(&mut self, recording: Id, rating: Option<u32>, cx: &mut Context<Self>) {
        let Some(key) = self.key(cx) else { return };
        let session = self.net.clone();
        self.rated.start(recording, key, rating);
        cx.notify();
        let sent = self.send::<RatingsSet>(
            RatingSetParams {
                idempotency_key: key,
                recording_id: recording,
                rating,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let answer = sent.await;
            this.update(cx, |player, cx| {
                let current_edit = player.rated.finish(recording, key);
                if let Some(set) = answer {
                    if session
                        .as_ref()
                        .zip(player.net.as_ref())
                        .is_some_and(|(from, current)| from.same_session(current))
                        && let Some(revision) = set.receipt.revision
                    {
                        player.rated.confirm(recording, set.rating, revision);
                        if set.recording_id != recording {
                            player.rated.confirm(set.recording_id, set.rating, revision);
                        }
                    }
                } else if current_edit {
                    player.say("The stars could not be saved", cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A record's songs, read as it is played, so that a group gives the
    /// release its page shows (`recordSongs`).
    pub(crate) fn record_songs(
        &mut self,
        album: AlbumRef,
        cx: &mut Context<Self>,
    ) -> Task<Option<(PlaySource, PlayContext)>> {
        let read = self.send::<AlbumGet>(AlbumGetParams { album }, cx);
        cx.spawn(async move |_, _| {
            let page = read.await?;
            Some((
                PlaySource::Tracks {
                    ids: page
                        .tracks
                        .iter()
                        .map(|track| track.appearance_id)
                        .collect(),
                },
                PlayContext {
                    kind: ContextKind::Album,
                    reference: Some(page.release_id),
                },
            ))
        })
    }

    /// An artist's songs, record by record, as a view their page's Play plays.
    fn artist_songs(
        &mut self,
        artist: Id,
        cx: &mut Context<Self>,
    ) -> Task<Option<(PlaySource, PlayContext, String)>> {
        let opened = self.send::<ListOpen>(
            ListOpenParams {
                kind: ListKind::Songs,
                query: Query {
                    filter: Some(Filter {
                        version: FILTER_VERSION,
                        predicate: Predicate::ByArtist { artist_id: artist },
                    }),
                    sort: Some(vec![Sort {
                        key: SortKey::Album,
                    }]),
                    ..Query::default()
                },
                window: None,
                replaces: None,
            },
            cx,
        );
        cx.spawn(async move |_, _| {
            let opened = opened.await?;
            Some((
                PlaySource::View {
                    view: opened.view.clone(),
                },
                PlayContext {
                    kind: ContextKind::Artist,
                    reference: Some(artist),
                },
                opened.view,
            ))
        })
    }

    /// What a record's or an artist's menu does: queue it, play it or shuffle it.
    pub(crate) fn many(&mut self, many: Many, doing: Doing, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let read = match &many {
                Many::Record { album, .. } => this
                    .update(cx, |player, cx| player.record_songs(*album, cx))
                    .ok()?
                    .await
                    .map(|(source, context)| (source, context, None)),
                Many::Artist { id, .. } => this
                    .update(cx, |player, cx| player.artist_songs(*id, cx))
                    .ok()?
                    .await
                    .map(|(source, context, view)| (source, context, Some(view))),
            };
            let (source, context, view) = read?;
            let name = format!("“{}”", many.name());
            let done = this
                .update(cx, |player, cx| match doing {
                    Doing::Next => player.add(source, QueuePlace::Next, name, Some(context), cx),
                    Doing::Last => player.add(source, QueuePlace::End, name, Some(context), cx),
                    Doing::Play => player.play(source, None, Some(context), false, cx),
                    Doing::Shuffle => player.play(source, None, Some(context), true, cx),
                })
                .ok()?;
            done.await;
            if let Some(view) = view {
                this.update(cx, |player, cx| {
                    player.send::<ListClose>(ViewParams { view }, cx).detach();
                })
                .ok();
            }
            Some(())
        })
        .detach();
    }
}

/// A thing of many songs, as its menu plays it.
#[derive(Debug, Clone)]
pub(crate) enum Many {
    Record { album: AlbumRef, name: String },
    Artist { id: Id, name: String },
}

impl Many {
    fn name(&self) -> &str {
        match self {
            Self::Record { name, .. } | Self::Artist { name, .. } => name,
        }
    }
}

/// What a menu's item asks of a thing of many songs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Doing {
    Next,
    Last,
    Play,
    Shuffle,
}

impl Doing {
    /// The item of the record's and the artist's menus, by its words.
    pub(crate) fn named(item: &str) -> Option<Self> {
        Some(match item {
            "Play Next" => Self::Next,
            "Play Last" => Self::Last,
            "Play" => Self::Play,
            "Shuffle" => Self::Shuffle,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_short_wait_for_audio_shows_nothing_then_breathes_from_nothing() {
        let ms = Duration::from_millis;
        assert_eq!(breath(ms(0)), 0.);
        assert_eq!(breath(ms(499)), 0.);
        assert!(breath(BUFFER_QUIET) < 1e-6);
        // A breath is 1.6 s: full at its middle, and back to nothing.
        assert!((breath(BUFFER_QUIET + ms(800)) - 1.).abs() < 1e-4);
        assert!(breath(BUFFER_QUIET + ms(1600)) < 1e-4);
        for at in (0..4000).step_by(37) {
            let b = breath(ms(at));
            assert!((0. ..=1.).contains(&b), "{at} ms: {b}");
        }
    }

    #[test]
    fn a_wait_keeps_its_start_until_it_ends() {
        let start = Instant::now();
        let later = start + Duration::from_secs(3);
        // Starting to wait starts the clock.
        assert_eq!(waiting_since(false, true, None, start), Some(start));
        // Still waiting, as on the next song, keeps it.
        assert_eq!(waiting_since(true, true, Some(start), later), Some(start));
        // Playing, paused or stopped, the wait is over at once.
        assert_eq!(waiting_since(true, false, Some(start), later), None);
        assert_eq!(waiting_since(false, false, None, later), None);
        // A wait that starts again starts over.
        assert_eq!(waiting_since(false, true, Some(start), later), Some(later));
    }

    /// A state of the player on `entry`, at `revision`, with `status`.
    fn state_of(revision: u64, status: &str, entry: Id) -> PlaybackState {
        serde_json::from_value(serde_json::json!({
            "revision": revision, "status": status, "positionMs": 0, "volume": 1.0,
            "muted": false, "shuffle": false, "repeat": "off", "queueRevision": 1,
            "queueLength": 2, "entryId": entry,
            "output": {"name": "null", "mode": "shared", "sampleRateHz": 44100, "channels": 2}
        }))
        .unwrap_or_else(|error| unreachable!("an invented state reads: {error}"))
    }

    #[gpui::test]
    fn only_visible_playing_rows_put_animation_in_the_scene(cx: &mut gpui::TestAppContext) {
        use crate::{tracks::Tracks, upnext::UpNextCard};
        use gpui::{AppContext, IntoElement, ParentElement, Render, Styled, Window, div, px};

        struct Page {
            tracks: Entity<Tracks>,
            card: Option<Entity<UpNextCard>>,
            scroll: f32,
        }

        impl Render for Page {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .flex()
                    .size_full()
                    .child(
                        div()
                            .relative()
                            .w(px(450.))
                            .h(px(100.))
                            .overflow_hidden()
                            .child(
                                div()
                                    .absolute()
                                    .top(px(-self.scroll))
                                    .child(self.tracks.clone()),
                            ),
                    )
                    .child(div().w(px(340.)).h_full().children(self.card.clone()))
            }
        }

        let song = Id::from_bytes([1; 16]);
        let link = cx.update(crate::link::unlinked);
        cx.update(|cx| cx.set_global(crate::marks::SceneClock(Instant::now())));
        let player = cx.new(|cx| Player::new(&link, cx));
        let tracks = cx.new(|cx| Tracks::new(player.clone(), false, cx));
        let row = serde_json::from_value(serde_json::json!({
            "appearanceId": song, "recordingId": song, "playCount": 0, "copies": [], "loved": false
        }))
        .unwrap_or_else(|error| unreachable!("an invented row reads: {error}"));
        tracks.update(cx, |tracks, cx| {
            tracks.width = 450.;
            let album = PlayContext {
                kind: ritornel_contract::ContextKind::Album,
                reference: None,
            };
            tracks.set(vec![row], 1, None, album, cx);
        });
        let card = cx.new(|cx| UpNextCard::new(link, player.clone(), cx));
        let (page, cx) = cx.add_window_view(|_, _| Page {
            tracks,
            card: Some(card),
            scroll: 0.,
        });
        let mut revision = 0;
        let mut playback = |status, cx: &mut gpui::VisualTestContext| {
            revision += 1;
            let mut state = state_of(revision, status, song);
            state.track = Some(
                serde_json::from_value(serde_json::json!({"appearanceId": song}))
                    .unwrap_or_else(|error| unreachable!("an invented track reads: {error}")),
            );
            player.update(cx, |player, cx| player.took(state, cx));
        };
        let check = |expected, cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.draw(cx).clear(cx);
                let animated = window
                    .rendered_scene()
                    .monochrome_sprites
                    .iter()
                    .filter(|sprite| sprite.animation != gpui::SpriteAnimationId::NONE)
                    .count();
                assert_eq!(animated, expected, "retained equalizer sprites");
                assert_eq!(
                    window.simulate_next_frame(cx),
                    0,
                    "a view asked for a redraw"
                );
            });
        };

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert!(cx.update(|window, _| window.is_window_active()));
        playback("playing", cx);
        check(6, cx);
        cx.deactivate_window();
        assert!(!cx.update(|window, _| window.is_window_active()));
        check(6, cx);
        playback("paused", cx);
        check(0, cx);
        playback("playing", cx);
        check(6, cx);
        playback("stopped", cx);
        check(0, cx);
        playback("playing", cx);
        page.update(cx, |page, cx| {
            page.card = None;
            cx.notify();
        });
        check(3, cx);
        page.update(cx, |page, cx| {
            page.scroll = 110.;
            cx.notify();
        });
        check(0, cx);
        page.update(cx, |page, cx| {
            page.scroll = 0.;
            cx.notify();
        });
        check(3, cx);
        cx.update(|_, cx| cx.set_reduce_motion(true));
        check(0, cx);
    }

    #[gpui::test]
    fn a_wait_ends_with_the_first_sound_or_a_new_session(cx: &mut gpui::TestAppContext) {
        let (first, second) = (Id::from_bytes([1; 16]), Id::from_bytes([2; 16]));
        let link = cx.update(crate::link::unlinked);
        let player = gpui::AppContext::new(cx, |cx| Player::new(&link, cx));
        let waiting = |cx: &mut gpui::TestAppContext| {
            player.read_with(cx, |player, _| player.buffering().is_some())
        };
        player.update(cx, |player, cx| {
            player.took(state_of(1, "buffering", first), cx)
        });
        assert!(waiting(cx), "a buffering state starts the wait");
        // The next song still waits: the wait goes on.
        player.update(cx, |player, cx| {
            player.took(state_of(2, "buffering", second), cx)
        });
        assert!(waiting(cx));
        // Progress on another entry says nothing of this one.
        player.update(cx, |player, cx| {
            player.ticked(
                &ProgressEvent {
                    player: None,
                    entry_id: first,
                    position_ms: 0,
                    playing: true,
                },
                cx,
            );
        });
        assert!(waiting(cx));
        // Sound on this one ends it, before the state says so.
        player.update(cx, |player, cx| {
            player.ticked(
                &ProgressEvent {
                    player: None,
                    entry_id: second,
                    position_ms: 10,
                    playing: true,
                },
                cx,
            );
        });
        assert!(!waiting(cx), "the first sound ends the wait");
        player.update(cx, |player, cx| {
            player.took(state_of(3, "playing", second), cx)
        });
        assert!(!waiting(cx));
        // A new wait, then the session goes: it ends with it, timer and all.
        player.update(cx, |player, cx| {
            player.took(state_of(4, "buffering", first), cx)
        });
        assert!(waiting(cx));
        link.update(cx, |_, cx| cx.emit(LinkEvent::Away));
        player.read_with(cx, |player, _| {
            assert!(player.buffering().is_none());
            assert!(player.buffering_since.is_none() && player.buffering_wakes.is_none());
        });
        // And a wait in the next session starts afresh, with its timer.
        player.update(cx, |player, cx| {
            player.took(state_of(1, "buffering", first), cx)
        });
        player.read_with(cx, |player, _| {
            assert!(player.buffering().is_some() && player.buffering_wakes.is_some());
        });
    }

    #[test]
    fn a_long_wait_says_where_the_audio_comes_from() {
        assert_eq!(getting_from(Some("Kestrel")), "Getting it from Kestrel…");
        assert_eq!(getting_from(Some("  ")), "Getting it from the hub…");
        assert_eq!(getting_from(None), "Getting it from the hub…");
    }

    use super::*;

    #[test]
    fn an_initial_read_cannot_rewind_a_newer_event() -> Result<(), serde_json::Error> {
        let event: PlaybackState = serde_json::from_value(serde_json::json!({
            "revision": 12, "status": "paused", "positionMs": 2000, "volume": 1.0,
            "muted": false, "shuffle": false, "repeat": "off", "queueRevision": 1,
            "queueLength": 1, "output": {"name": "null", "mode": "shared", "sampleRateHz": 44100, "channels": 2}
        }))?;
        let mut kept = None;
        assert!(keep_state(&mut kept, event.clone()).is_some());
        let mut reply = event.clone();
        reply.position_ms = 0;
        for revision in [11, 12] {
            reply.revision = revision;
            assert!(keep_state(&mut kept, reply.clone()).is_none());
        }
        assert_eq!(kept, Some(event));
        assert!(keep_state(&mut None, reply).is_some());
        Ok(())
    }

    #[test]
    fn rating_replies_only_retire_their_own_edit() {
        let [song, first, second] = [1, 2, 3].map(|n| Id::from_bytes([n; 16]));
        let mut ratings = Ratings::default();
        ratings.start(song, first, Some(80));
        ratings.start(song, second, None);
        assert!(!ratings.finish(song, first));
        assert_eq!(ratings.own(song, Some(40), None), None);
        assert!(ratings.finish(song, second));
        assert_eq!(ratings.own(song, Some(40), None), Some(40));
    }

    #[test]
    fn a_receipt_bridges_the_row_refresh_until_a_newer_rating_change() {
        let [song, edit] = [1, 2].map(|n| Id::from_bytes([n; 16]));
        for event_first in [true, false] {
            let mut ratings = Ratings::default();
            ratings.start(song, edit, Some(80));
            if event_first {
                ratings.revision = 5;
            }
            assert!(ratings.finish(song, edit));
            ratings.confirm(song, Some(80), 5);
            ratings.revision = 5;
            assert_eq!(ratings.own(song, Some(20), None), Some(80));
            assert_eq!(clicked(4, ratings.own(song, Some(20), None)), None);
            ratings.revision = 6;
            ratings.confirm(song, Some(80), 5);
            assert_eq!(ratings.own(song, Some(40), None), Some(40));
        }
    }

    #[test]
    fn two_song_ratings_stay_visible_until_their_rows_refresh() {
        let [first, second] = [1, 2].map(|n| Id::from_bytes([n; 16]));
        let mut ratings = Ratings::default();
        ratings.confirm(first, Some(80), 5);
        ratings.confirm(second, None, 6);
        assert_eq!(ratings.own(first, None, Some(4)), Some(80));
        assert_eq!(ratings.own(second, Some(40), Some(4)), None);
        assert_eq!(clicked(4, ratings.own(first, None, Some(4))), None);
        assert_eq!(ratings.own(first, Some(20), Some(7)), Some(20));
    }

    #[test]
    fn the_position_is_carried_on_and_stops_at_the_end() {
        let second = Duration::from_millis(1000);
        assert_eq!(carried(5_000, second, Some(200_000)), 6_000);
        assert_eq!(carried(199_500, second, Some(200_000)), 200_000);
        assert_eq!(carried(5_000, second, None), 6_000);
    }

    #[test]
    fn a_set_sound_stands_while_any_set_is_unanswered_and_a_while_after() {
        let Ok(mut state) = serde_json::from_value::<PlaybackState>(serde_json::json!({
            "revision": 1, "status": "paused", "positionMs": 0, "volume": 0.2,
            "muted": false, "shuffle": false, "repeat": "off", "queueRevision": 1,
            "queueLength": 1, "output": {"name": "null", "mode": "shared", "sampleRateHz": 44100, "channels": 2}
        })) else {
            return;
        };
        let t0 = Instant::now();
        let set = SoundSet {
            id: 2,
            volume: Some(0.6),
            muted: false,
            at: t0,
        };
        // A late report of an earlier set, long after, while this one is unanswered.
        let late = t0 + Duration::from_secs(5);
        assert!(settled(&mut state, Some(set), 1, late));
        assert!((state.volume - 0.6).abs() < 1e-9);
        // Answered: a report of another sound still passes for a while.
        state.volume = 0.2;
        assert!(settled(
            &mut state,
            Some(set),
            0,
            t0 + Duration::from_millis(500)
        ));
        // Then the place's word stands, and so does one that confirms it.
        state.volume = 0.2;
        assert!(!settled(&mut state, Some(set), 0, late));
        assert!((state.volume - 0.2).abs() < 1e-9);
        state.volume = 0.6;
        assert!(!settled(&mut state, Some(set), 0, t0));
    }

    #[test]
    fn repeat_goes_off_all_one_and_off_again() {
        assert_eq!(next_repeat(Repeat::Off), Repeat::All);
        assert_eq!(next_repeat(Repeat::All), Repeat::One);
        assert_eq!(next_repeat(Repeat::One), Repeat::Off);
    }

    #[test]
    fn stars_are_a_fifth_of_the_rating_and_a_love_with_none_is_five() {
        assert_eq!(stars_of(Some(80), false), 4);
        assert_eq!(stars_of(Some(100), true), 5);
        assert_eq!(stars_of(Some(0), true), 0);
        assert_eq!(stars_of(None, true), 5);
        assert_eq!(stars_of(None, false), 0);
    }

    fn id_of(id: u128) -> Id {
        Id::from_bytes(id.to_be_bytes())
    }

    fn entry(id: u128) -> QueueEntry {
        QueueEntry {
            entry_id: id_of(id),
            track: ritornel_contract::TrackSummary {
                appearance_id: Id::from_bytes((id + 100).to_be_bytes()),
                recording_id: None,
                release_id: None,
                title: None,
                artist: None,
                album: None,
                disc_number: None,
                track_number: None,
                duration_ms: None,
                art_id: None,
                rating: None,
                loved: false,
            },
            failed_reason: None,
        }
    }

    fn ids(entries: &[QueueEntry]) -> Vec<u128> {
        entries
            .iter()
            .map(|entry| u128::from_be_bytes(*entry.entry_id.as_bytes()))
            .collect()
    }

    fn after(id: u128) -> QueuePlace {
        QueuePlace::After {
            entry_id: id_of(id),
        }
    }

    #[test]
    fn what_is_moved_goes_straight_after_the_entry_it_is_dropped_under() {
        let entries: Vec<QueueEntry> = (1..=4).map(entry).collect();
        let one = |id: u128| [id_of(id)];
        assert_eq!(ids(&moved(&entries, &one(4), after(1), true)), [1, 4, 2, 3]);
        assert_eq!(ids(&moved(&entries, &one(1), after(3), true)), [2, 3, 1, 4]);
    }

    #[test]
    fn songs_dropped_below_the_list_go_to_the_end_past_what_the_card_has_not_read() {
        let entries: Vec<QueueEntry> = (1..=4).map(entry).collect();
        let one = |id: u128| [id_of(id)];
        assert_eq!(move_to(QueuePlace::End, &one(1)), Some(QueuePlace::End));
        assert_eq!(move_to(after(2), &one(1)), Some(after(2)));
        assert_eq!(move_to(QueuePlace::Next, &one(3)), Some(QueuePlace::Next));
        assert_eq!(move_to(after(1), &one(1)), None);
        // Shown at once: at the end of a card that holds all of Up next,
        // and gone from one that holds only its first entries.
        assert_eq!(
            ids(&moved(&entries, &one(2), QueuePlace::End, true)),
            [1, 3, 4, 2]
        );
        assert_eq!(
            ids(&moved(&entries, &one(2), QueuePlace::End, false)),
            [1, 3, 4]
        );
    }

    #[test]
    fn what_is_moved_goes_to_the_top_when_dropped_above_every_entry() {
        let entries: Vec<QueueEntry> = (1..=4).map(entry).collect();
        let one = [id_of(3)];
        assert_eq!(
            ids(&moved(&entries, &one, QueuePlace::Next, true)),
            [3, 1, 2, 4]
        );
    }

    #[test]
    fn a_click_on_the_stars_a_song_has_takes_them_away() {
        assert_eq!(clicked(3, None), Some(60));
        assert_eq!(clicked(3, Some(80)), Some(60));
        assert_eq!(clicked(3, Some(60)), None);
        // Three and a half stars show as four: the fourth is the lit one.
        assert_eq!(clicked(4, Some(70)), None);
        assert_eq!(clicked(3, Some(70)), Some(60));
    }
}
