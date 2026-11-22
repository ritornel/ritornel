//! The player: one queue, one session and one listen recorder above a
//! renderer. See "Queue and session" and "Recording a listen" in
//! docs/internals/playback.md.
//!
//! This is the control side, and it owns all state. It does nothing on its
//! own: a command changes it, and a tick, which the server gives it a few
//! times a second, takes in what the output did, saves the listens when a
//! save is due, and moves on when a track's audio runs out. A test drives
//! both with a fake renderer and clocks it sets.
//!
//! Every command and every tick first takes in what the output said, so
//! nothing is decided on a track the output has left: one that ran into
//! the next has made the next current by then. Each span handed down is
//! registered with the listen it opens or carries on, as it is decided, and
//! the listens are folded from the output's events alone ([`Recorder`]).
//! Saving them is a step of its own, and one that fails only puts off the
//! next: what the player did is still said, and why saving fails is read
//! with [`Player::save_failing`].
//!
//! An output that cannot be opened, or that goes away, is no track's
//! fault: the player waits, paused where it is, and says why in its state
//! until play opens the output again.
//!
//! The entry that follows is prepared as soon as a track starts, and again
//! whenever an edit lets it go, and the renderer joins it with no gap. An
//! entry queued from an album plays at its album's ReplayGain, any other at
//! its track's. The volume is the player's own.
//!
//! The queue keeps its two orders in [`Queue`]; what edits it is in
//! `edits`. Shuffle is a stored play order, and repeat is off, all or one;
//! [`Player::following`] is where both decide what comes next.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ritornel_domain::history::{ContextKind, EndReason, StartReason};

use crate::playback::catalog::{Catalog, Copy, TrackDisplay};
use crate::playback::listen::{Recorder, Span};
use crate::playback::queue::{Queue, Random, Repeat};
use crate::playback::renderer::{End, Event, Format, Progress, RenderError, Renderer, ReplayGain};
use crate::playback::store::{Change, Entry, Started, Wrote};
use crate::timekeeping::Timekeeper;
use crate::{Clock, Command, Entropy, Error, Id, ServerStore, new_id};

mod edits;

pub use edits::{AddTracks, Order, Place, QueuePage, Settings};

/// How often an open listen is saved while it plays: every this much
/// heard.
pub const CHECKPOINT_EVERY_MS: i64 = 5_000;

/// How often a progress event goes out while playing.
const PROGRESS_EVERY_MS: i64 = 1_000;

/// Previous restarts the track when it is further in than this, and goes
/// back an entry otherwise.
const RESTART_AFTER_MS: i64 = 3_000;

/// Entries that fail one after another before the player stops.
const FAILURES_BEFORE_STOP: u32 = 3;

/// What the player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Nothing is playing, and play starts the entry from its beginning.
    Stopped,
    /// The entry is playing.
    Playing,
    /// The entry is paused where it was.
    Paused,
}

impl Status {
    /// The status as the contract writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Playing => "playing",
            Self::Paused => "paused",
        }
    }
}

/// Which way a skip goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// To the next entry.
    Next,
    /// To the previous one, or the start of this one.
    Previous,
}

/// What the player is given when it starts, besides the stores.
#[derive(Debug, Clone)]
pub struct PlayerSetup {
    /// This server, which every listen names as where it was played.
    pub server_id: Id,
    /// This run of the server. A listen names the machine's boot, which
    /// its monotonic clock counts from, and this run only where the
    /// machine's cannot be read.
    pub run: Id,
    /// The wall clock listens are stamped with.
    pub clock: Arc<dyn Clock>,
    /// The monotonic clock and the zone.
    pub timekeeper: Arc<dyn Timekeeper>,
    /// What listen IDs are made from.
    pub entropy: Arc<dyn Entropy>,
    /// How often an open listen is saved while it plays.
    pub checkpoint_every_ms: i64,
    /// For the crash test alone: stop with [`Error::FailPoint`] once the
    /// track playing passes this position, without saving anything.
    pub fail_at_position_ms: Option<i64>,
    /// For the record's tests alone: once the track playing passes this
    /// position, every save fails as a full disk's would, and playback
    /// goes on.
    pub fail_saves_after_ms: Option<i64>,
}

/// The track in an entry, with the copy that was opened for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlaying {
    /// The entry.
    pub entry_id: Id,
    /// Its place in the queue, from 0.
    pub index: usize,
    /// What the track shows, when the library still has it.
    pub track: Option<TrackDisplay>,
    /// The copy that is open, while one is.
    pub format: Option<Format>,
}

/// The player's state, whole: what a client is sent.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    /// Counts the changes to this state.
    pub revision: u64,
    /// What it is doing.
    pub status: Status,
    /// The entry it is on.
    pub current: Option<NowPlaying>,
    /// Where in that entry.
    pub position_ms: i64,
    /// How long the entry's track is, when that is known.
    pub duration_ms: Option<i64>,
    /// Counts the changes to the queue.
    pub queue_revision: i64,
    /// How many entries the queue has.
    pub queue_length: usize,
    /// Whether the queue plays in its stored shuffled order.
    pub shuffle: bool,
    /// Repeat.
    pub repeat: Repeat,
    /// The output: its name, rate and channels.
    pub output: (String, u32, u32),
    /// What is wrong with the output, while something is.
    pub output_failing: Option<OutputFailing>,
    /// The player's own volume, 0 to 1.
    pub volume: f64,
    /// Whether it is muted.
    pub muted: bool,
    /// Counts the jumps within an entry that were asked for: a seek, or a
    /// previous that starts the track again. Not kept across a restart.
    pub seeks: u64,
}

/// The output could not be opened, or went away: since when, and what it
/// said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFailing {
    /// When it first failed, as Unix milliseconds.
    pub since_ms: i64,
    /// What it said the last time, for display.
    pub detail: String,
}

/// What a command answers with once it has been carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// The queue's revision after it.
    pub queue_revision: i64,
    /// The entry the player is on after it.
    pub entry: Option<Id>,
}

/// What a client is told about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The state changed.
    State,
    /// Where the playing entry is, about once a second.
    Progress {
        /// The entry.
        entry: Id,
        /// Where in it.
        position_ms: i64,
        /// Whether it is playing.
        playing: bool,
    },
    /// The queue changed.
    Queue(i64),
    /// A track began to be heard, a gapless join to it included, or was
    /// heard again after a pause: what Last.fm's "now playing" is sent
    /// for. A seek within a track says nothing.
    Began {
        /// The track appearance.
        appearance: Id,
        /// The length of the copy playing.
        duration_ms: i64,
    },
    /// A listen was closed and waits to be delivered.
    ListenClosed,
    /// A track could not be played, or stopped before its end.
    Failed {
        /// The entry.
        entry: Id,
        /// Why, for the log.
        reason: String,
    },
}

/// What to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayTracks {
    /// The track appearances, in queue order.
    pub tracks: Vec<Id>,
    /// The one to start at, from 0. `None` is the first, or with shuffle
    /// on, any.
    pub start: Option<usize>,
    /// Why they were queued, and what from.
    pub context: (ContextKind, Option<Id>),
    /// The session that queued them.
    pub actor: Option<Id>,
    /// Shuffle on or off from now; `None` leaves it as it is.
    pub shuffle: Option<bool>,
    /// Whether the tracks were read from the library just now, as a view's
    /// are, and need not be looked up again.
    pub known: bool,
}

/// A command for the player, with its receipt.
pub struct Request {
    /// The command's ID and fingerprint.
    pub command: Command,
    /// The method, for the receipt.
    pub method: &'static str,
    /// What the command answers with, as JSON, from what it did.
    pub answer: Box<dyn FnOnce(Summary) -> String + Send>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("command", &self.command)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

/// What a command did: the answer to give, and the events to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    /// The answer, as JSON. For a command carried out before, the answer it
    /// was given then.
    pub answer: String,
    /// Whether it was carried out before and nothing was done now.
    pub replayed: bool,
    /// The events to send.
    pub events: Vec<PlayerEvent>,
}

/// A track that is loaded in the renderer.
#[derive(Debug, Clone)]
struct Loaded {
    /// The span playing it, and the output's rate it is counted at, kept
    /// for when the output has gone.
    span: u64,
    rate: u32,
    format: Format,
    duration_ms: i64,
    /// Where the span began.
    from_ms: i64,
    /// What a listen of it records, decided as it was loaded or prepared.
    listen: Started,
    /// The span's count, once it has ended.
    ended: Option<u64>,
}

/// The entry that follows the current one, opened to be joined to it:
/// `None` where it could not be, and it is then loaded when its turn comes.
#[derive(Debug, Clone)]
struct Prepared {
    entry_id: Id,
    loaded: Option<Loaded>,
    display: Option<TrackDisplay>,
}

/// The player.
#[derive(Debug)]
pub struct Player {
    setup: PlayerSetup,
    renderer: Box<dyn Renderer>,
    status: Status,
    queue: Queue,
    repeat: Repeat,
    current: Option<usize>,
    /// Where the current entry is while nothing is loaded.
    position_ms: i64,
    loaded: Option<Loaded>,
    /// The entry after it, opened to follow it with no gap.
    prepared: Option<Prepared>,
    display: Option<TrackDisplay>,
    /// Every listen, folded from what the output said.
    recorder: Recorder,
    /// How far the span in hand had played when the output was last asked.
    progress: Progress,
    /// How the loaded track's audio ran out, and whether any of it was
    /// heard: the tick moves on from it.
    ran_out: Option<(End, bool)>,
    /// The last span handed down, by its number.
    span: u64,
    /// Why saving fails, from the first save that failed until one works:
    /// what the record's sign says. The next save tries again.
    save_failing: Option<String>,
    /// What is wrong with the output, from when it first went or could
    /// not be opened until a track is opened on it.
    output_failing: Option<OutputFailing>,
    /// The listen a resume of the current entry continues.
    resume_listen: Option<Id>,
    /// Why the next track loaded starts.
    start_reason: StartReason,
    listener: Option<Id>,
    queue_revision: i64,
    revision: u64,
    seeks: u64,
    failures_in_a_row: u32,
    last_progress_mono: i64,
    volume: f64,
    muted: bool,
}

fn no_library() -> Error {
    Error::NoLibrary
}

/// This server's roots, by name.
fn roots(server: &ServerStore) -> Result<HashMap<String, PathBuf>, Error> {
    Ok(server
        .roots()?
        .into_iter()
        .map(|(name, path)| (name, PathBuf::from(path)))
        .collect())
}

/// The ReplayGain an entry plays at: its album's when it was queued as an
/// album, so the album keeps its own dynamics, and its track's otherwise.
fn gain_for(entry: &Entry) -> ReplayGain {
    match entry.context_kind {
        ContextKind::Album => ReplayGain::Album,
        _ => ReplayGain::Track,
    }
}

/// How long the copy opened is: as the library knows it, or failing that
/// as its container says.
fn duration_of(copy: &Copy, format: &Format) -> i64 {
    if copy.duration_ms > 0 {
        copy.duration_ms
    } else {
        format
            .duration_ms
            .and_then(|ms| i64::try_from(ms).ok())
            .unwrap_or(0)
    }
}

/// Whether a listen that ended for `reason` is what playing the entry again
/// continues: one cut off by a shutdown or by the output going away.
fn resumable(reason: EndReason) -> bool {
    matches!(reason, EndReason::Shutdown | EndReason::OutputLost)
}

impl Player {
    /// The player as the last run left it: any listen left open is closed
    /// as a crash, and the queue and the place in it are back, paused.
    /// Nothing plays until it is asked to. See "Recording a listen" in
    /// docs/internals/playback.md.
    pub fn restore(
        setup: PlayerSetup,
        mut renderer: Box<dyn Renderer>,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
    ) -> Result<(Self, Vec<Id>), Error> {
        let crashed = server.close_crashed_listens()?;
        let session = server.playback_session()?;
        let (volume, muted) = server.volume()?;
        renderer.set_volume(if muted { 0.0 } else { volume as f32 });
        let queue = Queue::new(server.queue()?, session.shuffle);
        let listener = match (session.listener, library) {
            (Some(listener), _) => Some(listener),
            (None, Some(library)) => {
                let first = library.first_listener()?;
                if let Some(first) = first {
                    server.set_listener(first)?;
                }
                first
            }
            (None, None) => None,
        };
        let current = session
            .current
            .and_then(|row| queue.iter().position(|entry| entry.row == row));
        let mono = setup.timekeeper.mono_ms();
        let mut player = Self {
            setup,
            renderer,
            status: if current.is_some() {
                Status::Paused
            } else {
                Status::Stopped
            },
            queue,
            repeat: session.repeat,
            current,
            position_ms: session.position_ms,
            loaded: None,
            prepared: None,
            display: None,
            recorder: Recorder::default(),
            progress: Progress::default(),
            ran_out: None,
            span: 0,
            save_failing: None,
            output_failing: None,
            resume_listen: session.resume_listen,
            start_reason: StartReason::UserPick,
            listener,
            queue_revision: session.queue_revision,
            revision: u64::try_from(session.state_revision).unwrap_or(0),
            seeks: 0,
            failures_in_a_row: 0,
            last_progress_mono: mono,
            volume,
            muted,
        };
        player.display = player.read_display(library);
        Ok((player, crashed))
    }

    fn read_display(&self, library: Option<&dyn Catalog>) -> Option<TrackDisplay> {
        let entry = self.entry()?;
        library?.track(entry.appearance).ok().flatten()
    }

    fn entry(&self) -> Option<&Entry> {
        self.queue.get(self.current?)
    }

    fn entry_row(&self) -> Option<i64> {
        self.entry().map(|entry| entry.row)
    }

    /// Where the current entry is, as far as the output had played when
    /// last asked.
    fn position(&self) -> i64 {
        let Some(loaded) = &self.loaded else {
            return self.position_ms;
        };
        let frames = match loaded.ended {
            Some(frames) => frames,
            None if self.progress.span == loaded.span => self.progress.frames,
            None => 0,
        };
        let heard = frames.saturating_mul(1000) / u64::from(loaded.rate);
        let heard = i64::try_from(heard).unwrap_or(i64::MAX);
        loaded
            .from_ms
            .saturating_add(heard)
            .min(loaded.duration_ms.max(loaded.from_ms))
    }

    /// The state, whole.
    pub fn state(&self) -> PlayerState {
        let current = self.current.and_then(|index| {
            let entry = self.queue.get(index)?;
            Some(NowPlaying {
                entry_id: entry.entry_id,
                index,
                track: self.display.clone(),
                format: self.loaded.as_ref().map(|loaded| loaded.format.clone()),
            })
        });
        PlayerState {
            revision: self.revision,
            status: self.status,
            current,
            position_ms: self.position(),
            duration_ms: match &self.loaded {
                Some(loaded) => Some(loaded.duration_ms),
                None => self.display.as_ref().and_then(|track| track.duration_ms),
            },
            queue_revision: self.queue_revision,
            queue_length: self.queue.len(),
            shuffle: self.queue.shuffled(),
            repeat: self.repeat,
            output: (
                self.renderer.name(),
                self.renderer.rate(),
                self.renderer.channels(),
            ),
            output_failing: self.output_failing.clone(),
            volume: self.volume,
            muted: self.muted,
            seeks: self.seeks,
        }
    }

    /// The listen that is open, by its ID.
    pub fn open_listen(&self) -> Option<Id> {
        self.recorder.open().map(|listen| listen.started.listen_id)
    }

    /// Whether the loaded track's audio has run out, and the next tick
    /// moves on from it.
    #[cfg(test)]
    pub(crate) fn moving_on(&self) -> bool {
        self.ran_out.is_some()
    }

    fn summary(&self) -> Summary {
        Summary {
            queue_revision: self.queue_revision,
            entry: self.entry().map(|entry| entry.entry_id),
        }
    }

    fn changed(&mut self, events: &mut Vec<PlayerEvent>) {
        self.revision += 1;
        if !events.contains(&PlayerEvent::State) {
            events.push(PlayerEvent::State);
        }
        if let Some(entry) = self.entry() {
            events.push(PlayerEvent::Progress {
                entry: entry.entry_id,
                position_ms: self.position(),
                playing: self.status == Status::Playing,
            });
        }
    }

    /// Takes in what the output did since it was last asked, then saves the
    /// listens if a save is due. Every command and tick does this first.
    pub(super) fn sync(&mut self, server: &ServerStore, events: &mut Vec<PlayerEvent>) {
        self.take_in(events);
        self.save(server, events);
    }

    /// Folds what the output said into the listens, in order, and follows
    /// the loaded track: one that ran into the track prepared after it
    /// makes that the current entry, and one whose audio ran out is moved
    /// on from by the tick.
    fn take_in(&mut self, events: &mut Vec<PlayerEvent>) {
        let progress = self.renderer.progress();
        let said = self.renderer.drain();
        // One reading of each clock dates all that was said. A stamp can
        // be later than now: the output stamps a buffer as it will play.
        let now = i128::from(self.renderer.now());
        let (mono, wall) = (self.setup.timekeeper.mono_ms(), self.setup.clock.now_ms());
        let time = |at: u64| {
            let ago = (now - i128::from(at)).div_euclid(1_000_000);
            let ago = i64::try_from(ago).unwrap_or(i64::MAX);
            (wall.saturating_sub(ago), mono.saturating_sub(ago))
        };
        for event in &said {
            let closed = self.recorder.last_closed();
            let open = self.recorder.open().map(|listen| listen.started.listen_id);
            self.recorder.fold(event, &time);
            let heard = self.recorder.last_closed() != closed;
            self.follow(event, heard, events);
            // A new listen, not a seek within the one open.
            if matches!(event, Event::Began { .. })
                && let Some(began) = self
                    .began()
                    .filter(|_| self.recorder.open().map(|listen| listen.started.listen_id) != open)
            {
                events.push(began);
            }
        }
        self.progress = progress;
    }

    /// What the open listen is heard as, to say it began.
    fn began(&self) -> Option<PlayerEvent> {
        self.recorder.open().map(|listen| PlayerEvent::Began {
            appearance: listen.started.appearance,
            duration_ms: listen.started.track_duration_ms,
        })
    }

    fn follow(&mut self, event: &Event, heard: bool, events: &mut Vec<PlayerEvent>) {
        let Event::Ended {
            span, frames, end, ..
        } = event
        else {
            return;
        };
        let Some(loaded) = self.loaded.as_mut().filter(|loaded| loaded.span == *span) else {
            return;
        };
        loaded.ended = Some(*frames);
        match end {
            End::Joined { next } => self.joined(*next, events),
            End::Drained | End::Failed(_) => self.ran_out = Some((end.clone(), heard)),
            End::Replaced => {}
        }
    }

    /// The track in hand ran into the one prepared after it, which plays
    /// now. The entry is found by its ID wherever an edit has put it since;
    /// one taken out of the queue meanwhile is let go by the next tick.
    fn joined(&mut self, next: u64, events: &mut Vec<PlayerEvent>) {
        let prepared = self.prepared.take();
        let index = prepared
            .as_ref()
            .and_then(|prepared| self.queue.index_of(prepared.entry_id));
        match (prepared, index) {
            (
                Some(Prepared {
                    loaded: Some(loaded),
                    display,
                    ..
                }),
                Some(index),
            ) if loaded.span == next => {
                self.loaded = Some(loaded);
                self.display = display;
                self.current = Some(index);
                self.position_ms = 0;
                self.resume_listen = None;
                self.failures_in_a_row = 0;
                self.changed(events);
            }
            _ => self.ran_out = Some((End::Joined { next }, false)),
        }
    }

    /// Saves the listens and the player's place, if a save is due: a listen
    /// opened or closed, was sought or paused, or has played five seconds
    /// since. One that fails is tried again at the next.
    fn save(&mut self, server: &ServerStore, events: &mut Vec<PlayerEvent>) {
        let live = self.recorder.live(self.progress);
        if !self.recorder.due(live, self.setup.checkpoint_every_ms) {
            return;
        }
        let place = (self.entry_row(), self.position(), self.resume_listen);
        let mono = self.setup.timekeeper.mono_ms();
        let saved = match self.setup.fail_saves_after_ms {
            Some(limit) if self.position() >= limit => Err(Error::Io(std::io::Error::other(
                "a test asked for saves to fail",
            ))),
            _ => server.save_listens(self.recorder.unsaved(live), place, mono),
        };
        match saved {
            Ok(()) => {
                self.save_failing = None;
                if self.recorder.saved(live) {
                    events.push(PlayerEvent::ListenClosed);
                }
            }
            Err(error) => self.save_failing = Some(error.to_string()),
        }
    }

    /// Why the player's saves are failing, while they are: from the first
    /// that failed until one works. Nothing else that goes wrong in a tick
    /// is said here.
    pub fn save_failing(&self) -> Option<&str> {
        self.save_failing.as_deref()
    }

    /// What a listen of `copy` for `entry` records, all but when it starts:
    /// decided now, as the track is loaded or prepared.
    fn facts(&self, entry: &Entry, copy: &Copy, reason: StartReason) -> Result<Started, Error> {
        Ok(Started {
            listen_id: new_id(self.setup.clock.now_ms(), &*self.setup.entropy)?,
            entry: Some(entry.entry_id),
            listener: self.listener.ok_or(no_library())?,
            actor_session: entry.added_by,
            played_on_server: self.setup.server_id,
            appearance: entry.appearance,
            asset: copy.asset,
            source_item: copy.source_item,
            source_revision: copy.revision_no,
            started_at_ms: 0,
            zone: self.setup.timekeeper.zone(),
            started_mono_ms: 0,
            boot: crate::timekeeping::clock_boot(&*self.setup.timekeeper, self.setup.run),
            start_reason: reason,
            continues: self.resume_listen.filter(|_| reason == StartReason::Resume),
            start_position_ms: 0,
            track_duration_ms: copy.duration_ms,
            context_kind: entry.context_kind,
            context_ref: entry.context_ref,
            shuffle: self.queue.shuffled(),
        })
    }

    /// The number of a span about to be handed down.
    fn next_span(&mut self) -> u64 {
        self.span += 1;
        self.span
    }

    /// Says what a span handed down is, before the output can say anything
    /// of it.
    fn register(&mut self, span: u64, listen: &Started, from_ms: i64, continues: Option<Id>) {
        let spec = Span {
            listen: listen.clone(),
            continues,
            from_ms,
            rate: self.renderer.rate().max(1),
            end: None,
        };
        self.recorder.register(span, spec);
    }

    /// Leaves the current entry's track for `reason`: notes it as why the
    /// listen ends, lets the renderer go of the track and of one prepared
    /// after it, and takes in what the output said of both. A listen cut
    /// off by a shutdown or the output going away is what playing the
    /// entry again continues; any other end leaves nothing to continue.
    ///
    /// The track prepared is let go only once that is taken in: should the
    /// output have run on into it meanwhile, it is the entry left, and the
    /// place saved is in it.
    fn leave(&mut self, server: &ServerStore, reason: EndReason, events: &mut Vec<PlayerEvent>) {
        let before = self.recorder.last_closed();
        self.recorder.note(reason);
        if self.loaded.is_some() {
            self.renderer.unload();
            self.take_in(events);
            self.position_ms = self.position();
            self.loaded = None;
        }
        self.prepared = None;
        self.ran_out = None;
        match self
            .recorder
            .last_closed()
            .filter(|_| self.recorder.last_closed() != before)
        {
            Some((listen, ended)) => self.resume_listen = resumable(ended).then_some(listen),
            None if !resumable(reason) => self.resume_listen = None,
            None => {}
        }
        self.save(server, events);
    }

    /// Opens the current entry at the current position and plays it. An
    /// entry none of whose copies opens is marked failed and the next is
    /// tried, and the player stops after three such entries in a row. An
    /// output that cannot be opened leaves the entry as it is, and the
    /// player paused on it where it was.
    fn start(
        &mut self,
        server: &ServerStore,
        library: &dyn Catalog,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        let roots = roots(server)?;
        loop {
            let Some(entry) = self.entry().cloned() else {
                self.status = Status::Stopped;
                return Ok(());
            };
            let opened = self.open(&entry, library, &roots)?;
            self.display = library.track(entry.appearance)?;
            match opened {
                Ok(loaded) => {
                    self.loaded = Some(loaded);
                    self.output_failing = None;
                    // Before it plays, so that its decoder has the next
                    // before it can reach its end, however short it is.
                    self.prepare_next(server, Some(library))?;
                    self.renderer.play();
                    self.status = Status::Playing;
                    self.failures_in_a_row = 0;
                    self.last_progress_mono = self.setup.timekeeper.mono_ms();
                    return Ok(());
                }
                Err(failure) if failure.is_output() => {
                    self.output_failed(&failure, events);
                    // Where it waits, should it go down before it plays.
                    return server.save_place(
                        self.entry_row(),
                        self.position_ms,
                        self.resume_listen,
                    );
                }
                Err(failure) => {
                    events.push(PlayerEvent::Failed {
                        entry: entry.entry_id,
                        reason: failure.to_string(),
                    });
                    self.mark_failed(server, failure.reason, events)?;
                    self.failures_in_a_row += 1;
                    let next = self.current.and_then(|index| self.queue.after(index));
                    // With repeat all the last goes round to the first, as at
                    // the end of a track, but never to itself.
                    let round =
                        next.is_none() && self.repeat == Repeat::All && self.queue.len() > 1;
                    if self.failures_in_a_row >= FAILURES_BEFORE_STOP || (next.is_none() && !round)
                    {
                        self.status = Status::Stopped;
                        self.position_ms = 0;
                        server.save_place(self.entry_row(), 0, None)?;
                        return Ok(());
                    }
                    if round {
                        self.status = Status::Playing;
                        self.position_ms = 0;
                        self.resume_listen = None;
                        return self.wrap(StartReason::AutoAdvance, server, library, events);
                    }
                    self.current = next;
                    self.position_ms = 0;
                    self.resume_listen = None;
                    self.start_reason = StartReason::AutoAdvance;
                }
            }
        }
    }

    /// Opens the first copy of an entry that works, as a span of its own.
    fn open(
        &mut self,
        entry: &Entry,
        library: &dyn Catalog,
        roots: &HashMap<String, PathBuf>,
    ) -> Result<Result<Loaded, RenderError>, Error> {
        let copies = match library.copies(entry.appearance, self.setup.server_id) {
            Ok(copies) => copies,
            Err(error) => return Ok(Err(RenderError::new("unreadable", error.to_string()))),
        };
        self.renderer.set_replay_gain(gain_for(entry));
        let mut failure = RenderError::new("no_copy_reachable", "no file of this track is here");
        let from_ms = self.position_ms.max(0);
        for copy in copies {
            let Some(path) = copy.path(roots) else {
                continue;
            };
            let listen = self.facts(entry, &copy, self.start_reason)?;
            let span = self.next_span();
            let start = u64::try_from(from_ms).unwrap_or(0);
            match self.renderer.load(&path, copy.facts, start, span) {
                Ok(format) => {
                    let duration_ms = duration_of(&copy, &format);
                    let listen = Started {
                        track_duration_ms: duration_ms,
                        ..listen
                    };
                    self.register(span, &listen, from_ms, None);
                    return Ok(Ok(Loaded {
                        span,
                        format,
                        duration_ms,
                        from_ms,
                        listen,
                        ended: None,
                        rate: self.renderer.rate().max(1),
                    }));
                }
                // No other copy opens on an output that does not.
                Err(error) if error.is_output() => return Ok(Err(error)),
                Err(error) => failure = error,
            }
        }
        Ok(Err(failure))
    }

    /// Moves to the entry at `index` from its beginning, playing it if the
    /// player was playing.
    fn go_to(
        &mut self,
        index: usize,
        reason: StartReason,
        server: &ServerStore,
        library: &dyn Catalog,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        let playing = self.status == Status::Playing;
        self.current = Some(index);
        self.position_ms = 0;
        self.resume_listen = None;
        self.start_reason = reason;
        if playing {
            self.start(server, library, events)?;
        } else {
            self.display = self.read_display(Some(library));
        }
        server.save_place(self.entry_row(), 0, None)?;
        Ok(())
    }

    /// The command's stored answer, if it was carried out before.
    fn replayed(server: &ServerStore, request: &Request) -> Result<Option<Done>, Error> {
        Ok(server.stored_result(request.command)?.map(|answer| Done {
            answer,
            replayed: true,
            events: Vec::new(),
        }))
    }

    /// Records a transport command's receipt with the player's place.
    fn receipt(
        &self,
        server: &ServerStore,
        request: Request,
        events: Vec<PlayerEvent>,
    ) -> Result<Done, Error> {
        let answer = (request.answer)(self.summary());
        let place = (self.entry_row(), self.position(), self.resume_listen);
        server.record_command(request.command, request.method, answer.clone(), Some(place))?;
        Ok(Done {
            answer,
            replayed: false,
            events,
        })
    }

    /// Takes the library's first listener as the one listening, if there
    /// is none yet: whoever plays is heard as someone.
    fn find_listener(&mut self, server: &ServerStore, library: &dyn Catalog) -> Result<(), Error> {
        if self.listener.is_none() {
            self.listener = library.first_listener()?;
            if let Some(listener) = self.listener {
                server.set_listener(listener)?;
            }
        }
        Ok(())
    }

    /// Replaces the queue with these tracks and plays the one at `start`.
    /// A listen that was open ends as `replaced`.
    pub fn play(
        &mut self,
        request: Request,
        what: PlayTracks,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
    ) -> Result<Done, Error> {
        let PlayTracks {
            tracks,
            start,
            context,
            actor,
            shuffle,
            known,
        } = what;
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let library = library.ok_or(no_library())?;
        if tracks.is_empty() {
            return Err(Error::Invalid("there is nothing to play"));
        }
        let mut events = Vec::new();
        self.sync(server, &mut events);
        if start.is_some_and(|start| start >= tracks.len()) {
            return Err(Error::Invalid("start is past the end of the tracks"));
        }
        if !known && library.unknown_appearance(&tracks)?.is_some() {
            return Err(Error::NotFound("no such track"));
        }
        self.find_listener(server, library)?;
        let shuffled = shuffle.unwrap_or(self.queue.shuffled());
        let mut random = Random::new(&*self.setup.entropy);
        let start = match start {
            Some(start) => start,
            None if shuffled => random.below(tracks.len())?,
            None => 0,
        };
        let entries = self.fresh(server, tracks, context, actor)?;
        let first = entries.get(start).map(|entry| (entry.row, entry.entry_id));
        let write = self
            .queue
            .replace(entries, Some(start), shuffled, &mut random)?;
        self.leave(server, EndReason::Replaced, &mut events);
        let change = Change {
            command: None,
            expected: None,
            queue: write,
            bump: true,
            repeat: None,
            volume: None,
            place: (first.map(|(row, _)| row), 0, None),
        };
        let answer = match self.commit(server, Some(request), change, first.map(|(_, id)| id))? {
            Wrote::Done { answer, .. } => answer,
            Wrote::Replayed(answer) => {
                return Ok(Done {
                    answer,
                    replayed: true,
                    events,
                });
            }
        };
        self.position_ms = 0;
        self.resume_listen = None;
        self.start_reason = StartReason::UserPick;
        self.failures_in_a_row = 0;
        events.push(PlayerEvent::Queue(self.queue_revision));
        self.start(server, library, &mut events)?;
        self.changed(&mut events);
        Ok(Done {
            answer,
            replayed: false,
            events,
        })
    }

    /// Pauses. The listen stays open across a pause of any length.
    pub fn pause(&mut self, request: Request, server: &ServerStore) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let mut events = Vec::new();
        self.sync(server, &mut events);
        if self.status == Status::Playing {
            self.renderer.pause();
            self.recorder.paused();
            self.status = Status::Paused;
            self.sync(server, &mut events);
            self.changed(&mut events);
        }
        self.receipt(server, request, events)
    }

    /// Plays again after a pause, or after a stop from the entry's
    /// beginning, or after a restart from where the last run left it, as a
    /// new listen that continues the one the restart closed.
    pub fn resume(
        &mut self,
        request: Request,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let mut events = Vec::new();
        self.sync(server, &mut events);
        match (self.status, &self.loaded) {
            (Status::Playing, _) => {}
            (Status::Paused, Some(_)) => {
                self.renderer.play();
                self.status = Status::Playing;
                events.extend(self.began());
                self.changed(&mut events);
            }
            _ if self.current.is_some() => {
                let library = library.ok_or(no_library())?;
                self.start_reason = if self.resume_listen.is_some() {
                    StartReason::Resume
                } else {
                    StartReason::UserPick
                };
                self.failures_in_a_row = 0;
                self.start(server, library, &mut events)?;
                self.changed(&mut events);
            }
            _ => {}
        }
        self.receipt(server, request, events)
    }

    /// Stops: the listen ends as `stopped`, and play starts the entry again
    /// from its beginning.
    pub fn stop(&mut self, request: Request, server: &ServerStore) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let mut events = Vec::new();
        self.sync(server, &mut events);
        if self.status != Status::Stopped {
            self.leave(server, EndReason::Stopped, &mut events);
            self.status = Status::Stopped;
            self.position_ms = 0;
            self.resume_listen = None;
            self.changed(&mut events);
        }
        self.receipt(server, request, events)
    }

    /// Moves the entry named to `position_ms`. A seek that names another
    /// entry than the current one changes nothing, so a late retry cannot
    /// move the wrong track. A track that cannot be moved in, because its
    /// file has changed or cannot be read, ends there, and the player moves
    /// on as it does from any track it cannot play.
    pub fn seek(
        &mut self,
        request: Request,
        entry: Id,
        position_ms: i64,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let mut events = Vec::new();
        self.sync(server, &mut events);
        if self
            .entry()
            .is_some_and(|current| current.entry_id == entry)
        {
            self.seek_to(position_ms, EndReason::Jump, server, library, &mut events)?;
            self.changed(&mut events);
        }
        self.receipt(server, request, events)
    }

    /// Plays the loaded track from `position_ms`, as a span that carries on
    /// its listen. A track the output ran on into just before is cut off
    /// for `cut`.
    fn seek_to(
        &mut self,
        position_ms: i64,
        cut: EndReason,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        self.seeks += 1;
        let Some(loaded) = self.loaded.clone() else {
            // Nothing is open to say how long it is, but the library may
            // know.
            let end = self
                .display
                .as_ref()
                .and_then(|track| track.duration_ms)
                .unwrap_or(i64::MAX);
            self.position_ms = position_ms.clamp(0, end.max(0));
            return Ok(());
        };
        let target = position_ms.clamp(0, loaded.duration_ms);
        // Should the listen have ended by the time it begins, it opens
        // one of its own.
        let listen = Started {
            listen_id: new_id(self.setup.clock.now_ms(), &*self.setup.entropy)?,
            ..loaded.listen.clone()
        };
        let span = self.next_span();
        self.recorder.note(cut);
        let to = u64::try_from(target).unwrap_or(0);
        if let Err(failure) = self.renderer.seek(loaded.span, to, span) {
            return self.fail_track(server, library, &failure, events);
        }
        let continues = self.recorder.open().map(|open| open.started.listen_id);
        self.register(span, &listen, target, continues);
        self.loaded = Some(Loaded {
            span,
            from_ms: target,
            ended: None,
            ..loaded
        });
        // The renderer let a track queued after this one go: it is
        // prepared again at once.
        self.prepared = None;
        self.ran_out = None;
        self.sync(server, events);
        self.prepare_next(server, library)
    }

    /// Skips from the entry named. A skip that names another entry than
    /// the current one changes nothing, so a retried skip cannot skip
    /// twice. Previous restarts the track when it is more than three
    /// seconds in.
    pub fn skip(
        &mut self,
        request: Request,
        from: Id,
        direction: Direction,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let mut events = Vec::new();
        self.sync(server, &mut events);
        let on_it = self.entry().is_some_and(|current| current.entry_id == from);
        if let (true, Some(index)) = (on_it, self.current) {
            let library = library.ok_or(no_library())?;
            let before = match self.repeat {
                Repeat::All => self.queue.before(index).or_else(|| self.queue.last()),
                Repeat::Off | Repeat::One => self.queue.before(index),
            };
            match (direction, before) {
                (Direction::Next, _) => {
                    self.leave(server, EndReason::Next, &mut events);
                    match self.queue.after(index) {
                        Some(next) => {
                            self.go_to(next, StartReason::UserPick, server, library, &mut events)?;
                        }
                        None if self.repeat == Repeat::All => {
                            self.wrap(StartReason::UserPick, server, library, &mut events)?;
                        }
                        None => {
                            self.status = Status::Stopped;
                            self.position_ms = 0;
                        }
                    }
                }
                (Direction::Previous, Some(before)) if self.position() <= RESTART_AFTER_MS => {
                    self.leave(server, EndReason::Previous, &mut events);
                    self.go_to(before, StartReason::UserPick, server, library, &mut events)?;
                }
                (Direction::Previous, _) => {
                    let cut = EndReason::Previous;
                    self.seek_to(0, cut, server, Some(library), &mut events)?;
                }
            }
            self.changed(&mut events);
        }
        self.receipt(server, request, events)
    }

    /// Takes in what the output did and saves the listens when a save is
    /// due; moves on from a track whose audio ran out, or that went with
    /// the output; and prepares what follows. A save that fails does not
    /// stop it, and is not said here: see [`save_failing`](Self::save_failing).
    pub fn tick(
        &mut self,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
    ) -> Result<Vec<PlayerEvent>, Error> {
        let mut events = Vec::new();
        self.sync(server, &mut events);
        if let Some(limit) = self.setup.fail_at_position_ms
            && self.status == Status::Playing
            && self.position() >= limit
        {
            return Err(Error::FailPoint);
        }
        if let Some(why) = self.progress.lost
            && self.loaded.is_some()
        {
            let lost = RenderError::new("output", why);
            self.lose_output(server, &lost, &mut events)?;
            self.changed(&mut events);
            return Ok(events);
        }
        match self.ran_out.take() {
            Some((End::Failed(failed), heard)) => {
                self.leave(server, EndReason::Error, &mut events);
                if let Some(entry) = self.entry().map(|entry| entry.entry_id) {
                    if !heard {
                        self.mark_failed(server, "unreadable", &mut events)?;
                    }
                    events.push(PlayerEvent::Failed {
                        entry,
                        reason: failed,
                    });
                }
                self.advance(false, server, library, &mut events)?;
            }
            Some((End::Joined { .. }, _)) => {
                // It ran into a track taken out of the queue, which stops.
                self.leave(server, EndReason::Removed, &mut events);
                self.advance(true, server, library, &mut events)?;
            }
            Some(_) => {
                self.leave(server, EndReason::Finished, &mut events);
                self.advance(true, server, library, &mut events)?;
            }
            None => {}
        }
        self.prepare_next(server, library)?;
        let mono = self.setup.timekeeper.mono_ms();
        if self.status == Status::Playing
            && mono - self.last_progress_mono >= PROGRESS_EVERY_MS
            && let Some(entry) = self.entry().map(|entry| entry.entry_id)
        {
            self.last_progress_mono = mono;
            events.push(PlayerEvent::Progress {
                entry,
                position_ms: self.position(),
                playing: true,
            });
        }
        Ok(events)
    }

    /// Marks the current entry with why it could not be played, here and
    /// in the store, and says the queue changed.
    fn mark_failed(
        &mut self,
        server: &ServerStore,
        reason: &str,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        let Some(entry) = self.current.and_then(|index| self.queue.get_mut(index)) else {
            return Ok(());
        };
        server.mark_failed(entry.row, reason.to_owned())?;
        entry.failed_reason = Some(reason.to_owned());
        events.push(PlayerEvent::Queue(self.queue_revision));
        Ok(())
    }

    /// The output went away or could not be opened: no track is to blame,
    /// so none is marked and the player does not move on. It waits, paused where it
    /// is, and says why until a track opens on the output again.
    fn output_failed(&mut self, failure: &RenderError, events: &mut Vec<PlayerEvent>) {
        self.status = Status::Paused;
        let since_ms = self
            .output_failing
            .as_ref()
            .map_or_else(|| self.setup.clock.now_ms(), |failing| failing.since_ms);
        self.output_failing = Some(OutputFailing {
            since_ms,
            detail: failure.message.clone(),
        });
        if let Some(entry) = self.entry().map(|entry| entry.entry_id) {
            events.push(PlayerEvent::Failed {
                entry,
                reason: failure.to_string(),
            });
        }
    }

    /// The output went away, or stopped answering, with a track in hand:
    /// the listen ends there, and the player waits, paused at that place.
    /// Play opens the output again and continues the listen, as a resume
    /// after a restart does.
    fn lose_output(
        &mut self,
        server: &ServerStore,
        failure: &RenderError,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        self.leave(server, EndReason::OutputLost, events);
        self.output_failed(failure, events);
        server.save_place(self.entry_row(), self.position_ms, self.resume_listen)
    }

    /// The track in hand cannot be played on: what it played is counted
    /// and its listen ends as an error, the entry is marked with why, and
    /// the player moves on. Where the output is what failed, the track is
    /// not to blame, and the player waits on it instead.
    fn fail_track(
        &mut self,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
        failure: &RenderError,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        if failure.is_output() {
            return self.lose_output(server, failure, events);
        }
        self.leave(server, EndReason::Error, events);
        if let Some(entry) = self.entry().map(|entry| entry.entry_id) {
            events.push(PlayerEvent::Failed {
                entry,
                reason: failure.to_string(),
            });
            self.mark_failed(server, failure.reason, events)?;
        }
        self.advance(false, server, library, events)
    }

    /// Moves on from a track that ended, played to its end (`finished`)
    /// or not: to the same entry again with repeat one, to the next in play
    /// order, round to the first with repeat all, or to a stop at the end.
    fn advance(
        &mut self,
        finished: bool,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        // A track that failed is not played again, whatever repeat says.
        let next = if finished {
            self.following()
        } else {
            self.current.and_then(|index| self.queue.after(index))
        };
        match (next, library) {
            (Some(next), Some(library)) => {
                let reason = self.follows_by(next);
                self.go_to(next, reason, server, library, events)?;
            }
            // A failed entry goes round only to another, never to itself.
            (None, Some(library))
                if self.current.is_some()
                    && self.repeat == Repeat::All
                    && (finished || self.queue.len() > 1) =>
            {
                self.wrap(StartReason::AutoAdvance, server, library, events)?;
            }
            _ => {
                self.status = Status::Stopped;
                self.position_ms = 0;
                self.resume_listen = None;
                server.save_place(self.entry_row(), 0, None)?;
            }
        }
        self.changed(events);
        Ok(())
    }

    /// The entry that plays when the current one ends by itself: the same
    /// again with repeat one, the next in play order, or with repeat all
    /// the first again. At the end of a shuffled queue repeat all needs a
    /// new order first, so nothing follows yet, and `advance` writes one.
    fn following(&self) -> Option<usize> {
        let index = self.current?;
        match self.repeat {
            Repeat::One => Some(index),
            Repeat::All => self.queue.after(index).or_else(|| {
                (!self.queue.shuffled())
                    .then(|| self.queue.first())
                    .flatten()
            }),
            Repeat::Off => self.queue.after(index),
        }
    }

    /// Why the entry at `next` starts when the current one ends into it:
    /// again, with repeat one.
    fn follows_by(&self, next: usize) -> StartReason {
        if self.repeat == Repeat::One && self.current == Some(next) {
            StartReason::Repeat
        } else {
            StartReason::AutoAdvance
        }
    }

    /// Lets the entry prepared to follow the current one go, if an edit has
    /// changed what follows: the next tick prepares what follows now. It is
    /// kept while the same entry still follows for the same reason, so that
    /// an edit elsewhere costs no join, and where the output has run into it
    /// already, when the join takes it wherever it is.
    fn forget_prepared(&mut self) {
        let Some(prepared) = &self.prepared else {
            return;
        };
        let decided = prepared
            .loaded
            .as_ref()
            .map(|loaded| loaded.listen.start_reason);
        let follows = self.following().and_then(|index| {
            let entry = self.queue.get(index)?.entry_id;
            Some((entry, Some(self.follows_by(index))))
        });
        if follows == Some((prepared.entry_id, decided)) {
            return;
        }
        if self.renderer.cancel_next() {
            self.prepared = None;
        }
    }

    /// Opens the entry that follows, to be joined to the current one, as
    /// soon as there is a current one and nothing is prepared. Opened means
    /// its copy is chosen and held from now to its end, and what its listen
    /// records, its start reason and the shuffle among it, is decided now.
    fn prepare_next(
        &mut self,
        server: &ServerStore,
        library: Option<&dyn Catalog>,
    ) -> Result<(), Error> {
        let (Some(_), Some(library)) = (&self.loaded, library) else {
            return Ok(());
        };
        if self.prepared.is_some() || self.ran_out.is_some() {
            return Ok(());
        }
        let Some((entry, reason)) = self.following().and_then(|index| {
            let entry = self.queue.get(index)?.clone();
            Some((entry, self.follows_by(index)))
        }) else {
            return Ok(());
        };
        let roots = roots(server)?;
        let copies = library.copies(entry.appearance, self.setup.server_id)?;
        let display = library.track(entry.appearance)?;
        self.renderer.set_replay_gain(gain_for(&entry));
        let mut opened = None;
        for copy in copies {
            let Some(path) = copy.path(&roots) else {
                continue;
            };
            let listen = self.facts(&entry, &copy, reason)?;
            let span = self.next_span();
            match self.renderer.queue_next(&path, copy.facts, span) {
                Ok(Some(format)) => {
                    let duration_ms = duration_of(&copy, &format);
                    let listen = Started {
                        track_duration_ms: duration_ms,
                        ..listen
                    };
                    self.register(span, &listen, 0, None);
                    opened = Some(Loaded {
                        span,
                        format,
                        duration_ms,
                        from_ms: 0,
                        listen,
                        ended: None,
                        rate: self.renderer.rate().max(1),
                    });
                    break;
                }
                // It cannot be joined: it loads when its turn comes.
                Ok(None) => break,
                // Its turn tries every copy again and marks the entry.
                Err(_) => {}
            }
        }
        self.prepared = Some(Prepared {
            entry_id: entry.entry_id,
            loaded: opened,
            display,
        });
        Ok(())
    }

    /// Sets the player's own volume, 0 to 1, and whether it is muted; either
    /// left out stays as it is. See "Queue and session" in
    /// docs/internals/playback.md.
    pub fn set_volume(
        &mut self,
        request: Request,
        volume: Option<f64>,
        muted: Option<bool>,
        server: &ServerStore,
    ) -> Result<Done, Error> {
        let settings = Settings {
            volume,
            muted,
            ..Settings::default()
        };
        self.set(request, settings, server)
    }

    /// Stops for a clean shutdown: the open listen ends as `shutdown` and is
    /// what a resume after the restart continues, and the position is
    /// saved. The output is let go. Whether the listens were saved is
    /// [`save_failing`](Self::save_failing).
    pub fn shutdown(&mut self, server: &ServerStore) -> Result<Vec<PlayerEvent>, Error> {
        let mut events = Vec::new();
        self.sync(server, &mut events);
        self.renderer.pause();
        self.leave(server, EndReason::Shutdown, &mut events);
        if self.current.is_some() {
            self.status = Status::Paused;
            server.save_place(self.entry_row(), self.position_ms, self.resume_listen)?;
        }
        Ok(events)
    }
}
