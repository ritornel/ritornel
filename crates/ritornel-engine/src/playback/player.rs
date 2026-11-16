//! The player: one queue, one session and one listen recorder above a
//! renderer. See decisions 1, 8, 9, 18 to 22 and 24, "The queue" and
//! "Recording a listen" in docs/design/playback.md.
//!
//! This is the control side, and it owns all state. It does nothing on its
//! own: a command changes it, and a tick, which the server gives it a few
//! times a second, reads what the renderer has played, opens, checkpoints
//! and closes listens, and moves on at the end of a track. A test drives
//! both with a fake renderer and clocks it sets.
//!
//! This first player plays simply on purpose: a plain queue in the order it
//! was given, no gapless joins, no shuffle or repeat, no ReplayGain, no
//! volume (build-order.md, decision 2).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ritornel_domain::history::{ContextKind, EndReason, StartReason};

use crate::playback::catalog::{Copy, TrackDisplay};
use crate::playback::listen::OpenListen;
use crate::playback::renderer::{Format, RenderError, Renderer, Segment};
use crate::playback::store::{Ended, Entry, Mark, NewEntry, Replaced, Started};
use crate::timekeeping::Timekeeper;
use crate::{Clock, Command, Entropy, Error, Id, LibraryStore, ServerStore, new_id};

/// How often an open listen is saved while it plays.
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The output: its name, rate and channels.
    pub output: (String, u32, u32),
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
    /// The track appearances, in the order they are played.
    pub tracks: Vec<Id>,
    /// The one to start at, from 0.
    pub start: usize,
    /// Why they were queued, and what from.
    pub context: (ContextKind, Option<Id>),
    /// The session that queued them.
    pub actor: Option<Id>,
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
    copy: Copy,
    format: Format,
    duration_ms: i64,
    /// Where the current segment began.
    from_ms: i64,
    /// The current segment's frames that have been counted.
    frames: u64,
}

/// A listen that has ended and whose close has not committed yet.
#[derive(Debug)]
struct Closing {
    listen: OpenListen,
    ended: Ended,
}

/// The player.
#[derive(Debug)]
pub struct Player {
    setup: PlayerSetup,
    renderer: Box<dyn Renderer>,
    status: Status,
    queue: Vec<Entry>,
    current: Option<usize>,
    /// Where the current entry is while nothing is loaded.
    position_ms: i64,
    loaded: Option<Loaded>,
    display: Option<TrackDisplay>,
    listen: Option<OpenListen>,
    /// A listen whose first frames were heard and whose row could not be
    /// written yet: its start, and every frame heard since, held until the
    /// write commits and the recorder has them.
    starting: Option<(Started, u64)>,
    /// A listen that ended and is not closed yet, because the write that
    /// closes it failed: kept whole, with how and when it ended, until a
    /// retry commits it. Nothing moves on from it until then.
    closing: Option<Closing>,
    /// The listen a resume of the current entry continues.
    resume_listen: Option<Id>,
    /// Why the next listen to open began.
    start_reason: StartReason,
    listener: Option<Id>,
    queue_revision: i64,
    revision: u64,
    failures_in_a_row: u32,
    last_write_mono: i64,
    last_progress_mono: i64,
    /// When the renderer was last told to play: no frame can have been
    /// heard before it.
    played_since_mono: i64,
}

fn no_library() -> Error {
    Error::NoLibrary
}

impl Player {
    /// The player as the last run left it: any listen left open is closed
    /// as a crash, and the queue and the place in it are back, paused.
    /// Nothing plays until it is asked to. See decision 18 in
    /// docs/design/playback.md.
    pub fn restore(
        setup: PlayerSetup,
        renderer: Box<dyn Renderer>,
        server: &ServerStore,
        library: Option<&LibraryStore>,
    ) -> Result<(Self, Vec<Id>), Error> {
        let crashed = server.close_crashed_listens()?;
        let session = server.playback_session()?;
        let queue = server.queue()?;
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
            current,
            position_ms: session.position_ms,
            loaded: None,
            display: None,
            listen: None,
            starting: None,
            closing: None,
            resume_listen: session.resume_listen,
            start_reason: StartReason::UserPick,
            listener,
            queue_revision: session.queue_revision,
            revision: u64::try_from(session.state_revision).unwrap_or(0),
            failures_in_a_row: 0,
            last_write_mono: mono,
            last_progress_mono: mono,
            played_since_mono: mono,
        };
        player.display = player.read_display(library);
        Ok((player, crashed))
    }

    fn read_display(&self, library: Option<&LibraryStore>) -> Option<TrackDisplay> {
        let entry = self.entry()?;
        library?.track(entry.appearance).ok().flatten()
    }

    fn entry(&self) -> Option<&Entry> {
        self.queue.get(self.current?)
    }

    fn entry_row(&self) -> Option<i64> {
        self.entry().map(|entry| entry.row)
    }

    /// Where the current entry is.
    fn position(&self) -> i64 {
        match &self.loaded {
            Some(loaded) => {
                let heard = i64::try_from(
                    loaded.frames.saturating_mul(1000) / u64::from(self.renderer.rate().max(1)),
                )
                .unwrap_or(i64::MAX);
                (loaded.from_ms + heard).min(loaded.duration_ms.max(loaded.from_ms))
            }
            None => self.position_ms,
        }
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
            output: (
                self.renderer.name(),
                self.renderer.rate(),
                self.renderer.channels(),
            ),
        }
    }

    /// The queue, in order.
    pub fn queue(&self) -> Vec<(Id, Id, Option<String>)> {
        self.queue
            .iter()
            .map(|entry| {
                (
                    entry.entry_id,
                    entry.appearance,
                    entry.failed_reason.clone(),
                )
            })
            .collect()
    }

    /// The listen that is open, by its ID.
    pub fn open_listen(&self) -> Option<Id> {
        self.listen.as_ref().map(|listen| listen.started.listen_id)
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

    /// Counts what the renderer has played since it was last asked, and
    /// opens the listen when the first frame of the track has been heard.
    /// The renderer's count is taken in here even when the listen cannot be
    /// written: its frames are then held with the listen's start, and given
    /// to the recorder once the write commits, so none is lost and the
    /// start does not move.
    fn account(
        &mut self,
        server: &ServerStore,
        segment: &Segment,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        let Some(loaded) = &mut self.loaded else {
            return Ok(());
        };
        let fresh = segment.frames.saturating_sub(loaded.frames);
        loaded.frames = segment.frames.max(loaded.frames);
        if fresh == 0 && self.starting.is_none() {
            return Ok(());
        }
        let position = self.position();
        if let Some(listen) = &mut self.listen {
            listen.heard(fresh, position);
            return Ok(());
        }
        let (started, frames) = match self.starting.take() {
            Some((started, held)) => (started, held + fresh),
            None => match self.start_of(fresh)? {
                Some(started) => (started, fresh),
                None => return Ok(()),
            },
        };
        let mark = Mark {
            position_ms: started.start_position_ms,
            mono_ms: self.setup.timekeeper.mono_ms(),
        };
        if let Err(error) = server.open_listen(started.clone(), self.entry_row(), mark) {
            self.starting = Some((started, frames));
            return Err(error);
        }
        // Checkpoints run every few seconds from the first frame heard.
        self.last_write_mono = started.started_mono_ms;
        let mut listen = OpenListen::new(started, self.renderer.rate().max(1));
        listen.heard(frames, position);
        self.listen = Some(listen);
        events.push(PlayerEvent::State);
        Ok(())
    }

    /// The start of the listen whose first `frames` were just heard.
    fn start_of(&self, frames: u64) -> Result<Option<Started>, Error> {
        let (Some(loaded), Some(entry)) = (&self.loaded, self.entry()) else {
            return Ok(None);
        };
        let rate = self.renderer.rate().max(1);
        let mono = self.setup.timekeeper.mono_ms();
        // The first frame was heard this long ago: what has been heard
        // since, but never before the renderer was told to play, which an
        // output faster than real time would otherwise put it before.
        let heard_ms = i64::try_from(frames.saturating_mul(1000) / u64::from(rate))
            .unwrap_or(i64::MAX)
            .min(mono.saturating_sub(self.played_since_mono).max(0));
        let now = self.setup.clock.now_ms();
        let resumes = self.start_reason == StartReason::Resume;
        let started = Started {
            listen_id: new_id(now, &*self.setup.entropy)?,
            entry: Some(entry.entry_id),
            listener: self.listener.ok_or(no_library())?,
            actor_session: entry.added_by,
            played_on_server: self.setup.server_id,
            appearance: entry.appearance,
            asset: loaded.copy.asset,
            source_item: loaded.copy.source_item,
            source_revision: loaded.copy.revision_no,
            started_at_ms: now.saturating_sub(heard_ms),
            zone: self.setup.timekeeper.zone(),
            started_mono_ms: mono.saturating_sub(heard_ms),
            boot: crate::timekeeping::clock_boot(&*self.setup.timekeeper, self.setup.run),
            start_reason: self.start_reason,
            continues: if resumes { self.resume_listen } else { None },
            start_position_ms: loaded.from_ms,
            track_duration_ms: loaded.duration_ms,
            context_kind: entry.context_kind,
            context_ref: entry.context_ref,
            shuffle: false,
        };
        Ok(Some(started))
    }

    /// Saves what the open listen has heard, and the position with it.
    fn checkpoint(&mut self, server: &ServerStore) -> Result<(), Error> {
        if let Some(listen) = &self.listen {
            let mono = self.setup.timekeeper.mono_ms();
            let mark = Mark {
                position_ms: listen.position_ms(),
                mono_ms: mono,
            };
            server.checkpoint_listen(listen.started.listen_id, listen.snapshot(), mark)?;
            self.last_write_mono = mono;
        }
        Ok(())
    }

    /// Closes the open listen, if there is one, for `reason`, and saves the
    /// place: a listen cut off by a shutdown is what a resume continues,
    /// and any other end leaves nothing to continue. A listen whose close
    /// failed before is closed first, as it ended then.
    fn end_listen(
        &mut self,
        server: &ServerStore,
        reason: EndReason,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        if self.closing.is_some() {
            return self.finish_closing(server, events);
        }
        let Some(listen) = self.listen.take() else {
            if reason != EndReason::Shutdown {
                self.resume_listen = None;
            }
            return Ok(());
        };
        let ended = Ended {
            end_position_ms: listen.position_ms(),
            end_reason: reason,
            ended_at_ms: self.setup.clock.now_ms(),
            ended_mono_ms: Some(self.setup.timekeeper.mono_ms()),
        };
        self.closing = Some(Closing { listen, ended });
        self.finish_closing(server, events)
    }

    /// Commits the close of the listen that has ended, if there is one.
    /// Until it commits the listen is kept whole, and every command and
    /// tick tries again before it does anything else.
    fn finish_closing(
        &mut self,
        server: &ServerStore,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        let Some(Closing { listen, ended }) = &self.closing else {
            return Ok(());
        };
        let reason = ended.end_reason;
        let resume = (reason == EndReason::Shutdown).then_some(listen.started.listen_id);
        let position = match reason {
            EndReason::Finished => 0,
            _ => ended.end_position_ms,
        };
        server.close_listen(
            listen.started.listen_id,
            listen.snapshot(),
            ended.clone(),
            (self.entry_row(), position, resume),
        )?;
        self.closing = None;
        self.resume_listen = resume;
        self.last_write_mono = self.setup.timekeeper.mono_ms();
        events.push(PlayerEvent::ListenClosed);
        Ok(())
    }

    /// Leaves the current entry's track for `reason`: lets the renderer go
    /// of it, counts what it played last, and closes the listen.
    fn leave(
        &mut self,
        server: &ServerStore,
        reason: EndReason,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        if self.loaded.is_some() {
            let last = self.renderer.unload();
            self.account(server, &last, events)?;
            self.position_ms = self.position();
            self.loaded = None;
        }
        self.end_listen(server, reason, events)
    }

    /// Opens the current entry at the current position and plays it. An
    /// entry none of whose copies opens is marked failed and the next is
    /// tried, and the player stops after three such entries in a row.
    fn start(
        &mut self,
        server: &ServerStore,
        library: &LibraryStore,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        let roots: HashMap<String, PathBuf> = server
            .roots()?
            .into_iter()
            .map(|(name, path)| (name, PathBuf::from(path)))
            .collect();
        loop {
            let Some(entry) = self.entry().cloned() else {
                self.status = Status::Stopped;
                return Ok(());
            };
            let opened = self.open(&entry, library, &roots);
            self.display = library.track(entry.appearance)?;
            match opened {
                Ok(loaded) => {
                    self.loaded = Some(loaded);
                    self.renderer.play();
                    self.played_since_mono = self.setup.timekeeper.mono_ms();
                    self.status = Status::Playing;
                    self.failures_in_a_row = 0;
                    self.last_progress_mono = self.setup.timekeeper.mono_ms();
                    return Ok(());
                }
                Err(failure) => {
                    events.push(PlayerEvent::Failed {
                        entry: entry.entry_id,
                        reason: failure.to_string(),
                    });
                    server.mark_failed(entry.row, failure.reason.to_owned())?;
                    if let Some(failed) = self.current.and_then(|index| self.queue.get_mut(index)) {
                        failed.failed_reason = Some(failure.reason.to_owned());
                    }
                    events.push(PlayerEvent::Queue(self.queue_revision));
                    self.failures_in_a_row += 1;
                    let next = self.current.map(|index| index + 1);
                    let has_next = next.is_some_and(|next| next < self.queue.len());
                    if self.failures_in_a_row >= FAILURES_BEFORE_STOP || !has_next {
                        self.status = Status::Stopped;
                        self.position_ms = 0;
                        server.save_place(self.entry_row(), 0, None)?;
                        return Ok(());
                    }
                    self.current = next;
                    self.position_ms = 0;
                    self.resume_listen = None;
                    self.start_reason = StartReason::AutoAdvance;
                }
            }
        }
    }

    /// Opens the first copy of an entry that works.
    fn open(
        &mut self,
        entry: &Entry,
        library: &LibraryStore,
        roots: &HashMap<String, PathBuf>,
    ) -> Result<Loaded, RenderError> {
        let copies = library
            .copies(entry.appearance, self.setup.server_id)
            .map_err(|error| RenderError::new("unreadable", error.to_string()))?;
        let mut failure = RenderError::new("no_copy_reachable", "no file of this track is here");
        for copy in copies {
            let Some(root) = roots.get(&copy.root) else {
                continue;
            };
            let path = root.join(&copy.rel_path);
            let start = u64::try_from(self.position_ms.max(0)).unwrap_or(0);
            match self.renderer.load(&path, copy.facts, start) {
                Ok(format) => {
                    let duration_ms = if copy.duration_ms > 0 {
                        copy.duration_ms
                    } else {
                        format
                            .duration_ms
                            .and_then(|ms| i64::try_from(ms).ok())
                            .unwrap_or(0)
                    };
                    return Ok(Loaded {
                        copy,
                        format,
                        duration_ms,
                        from_ms: self.position_ms.max(0),
                        frames: 0,
                    });
                }
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }

    /// Moves to the entry at `index` from its beginning, playing it if the
    /// player was playing.
    fn go_to(
        &mut self,
        index: usize,
        reason: StartReason,
        server: &ServerStore,
        library: &LibraryStore,
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

    /// Replaces the queue with these tracks and plays the one at `start`.
    /// A listen that was open ends as `replaced`.
    pub fn play(
        &mut self,
        request: Request,
        what: PlayTracks,
        server: &ServerStore,
        library: Option<&LibraryStore>,
    ) -> Result<Done, Error> {
        let PlayTracks {
            tracks,
            start,
            context,
            actor,
        } = what;
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let library = library.ok_or(no_library())?;
        if tracks.is_empty() {
            return Err(Error::Invalid("there is nothing to play"));
        }
        let mut events = Vec::new();
        self.finish_closing(server, &mut events)?;
        if start >= tracks.len() {
            return Err(Error::Invalid("start is past the end of the tracks"));
        }
        for track in &tracks {
            if library.track(*track)?.is_none() {
                return Err(Error::NotFound("no such track"));
            }
        }
        if self.listener.is_none() {
            self.listener = library.first_listener()?;
            if let Some(listener) = self.listener {
                server.set_listener(listener)?;
            }
        }
        self.leave(server, EndReason::Replaced, &mut events)?;
        let entries = tracks
            .into_iter()
            .map(|appearance| NewEntry {
                appearance,
                context_kind: context.0,
                context_ref: context.1,
                added_by: actor,
            })
            .collect();
        let answer = request.answer;
        let replaced = server.replace_queue(
            request.command,
            request.method,
            entries,
            start,
            move |queue, revision| {
                answer(Summary {
                    queue_revision: revision,
                    entry: queue.get(start).map(|entry| entry.entry_id),
                })
            },
        )?;
        let (queue, answer) = match replaced {
            Replaced::Done(queue, answer) => (queue, answer),
            Replaced::Replayed(answer) => {
                return Ok(Done {
                    answer,
                    replayed: true,
                    events,
                });
            }
        };
        self.queue = queue;
        self.queue_revision += 1;
        self.current = Some(start);
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
        self.finish_closing(server, &mut events)?;
        if self.status == Status::Playing {
            self.renderer.pause();
            let polled = self.renderer.poll();
            self.account(server, &polled, &mut events)?;
            if let Some(listen) = &mut self.listen {
                listen.pause();
            }
            self.checkpoint(server)?;
            self.status = Status::Paused;
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
        library: Option<&LibraryStore>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let mut events = Vec::new();
        self.finish_closing(server, &mut events)?;
        match (self.status, &self.loaded) {
            (Status::Playing, _) => {}
            (Status::Paused, Some(_)) => {
                self.renderer.play();
                self.played_since_mono = self.setup.timekeeper.mono_ms();
                self.status = Status::Playing;
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
        self.finish_closing(server, &mut events)?;
        if self.status != Status::Stopped {
            self.leave(server, EndReason::Stopped, &mut events)?;
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
        library: Option<&LibraryStore>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let mut events = Vec::new();
        self.finish_closing(server, &mut events)?;
        if self
            .entry()
            .is_some_and(|current| current.entry_id == entry)
        {
            self.seek_to(position_ms, server, library, &mut events)?;
            self.changed(&mut events);
        }
        self.receipt(server, request, events)
    }

    fn seek_to(
        &mut self,
        position_ms: i64,
        server: &ServerStore,
        library: Option<&LibraryStore>,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        let Some(duration) = self.loaded.as_ref().map(|loaded| loaded.duration_ms) else {
            self.position_ms = position_ms.max(0);
            return Ok(());
        };
        let target = position_ms.clamp(0, duration);
        let ended = match self.renderer.seek(u64::try_from(target).unwrap_or(0)) {
            Ok(ended) => ended,
            Err(failure) => return self.fail_track(server, library, &failure, events),
        };
        self.account(server, &ended, events)?;
        if let Some(listen) = &mut self.listen {
            listen.seek(target);
        }
        if let Some(loaded) = &mut self.loaded {
            loaded.from_ms = target;
            loaded.frames = 0;
        }
        self.checkpoint(server)
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
        library: Option<&LibraryStore>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let mut events = Vec::new();
        self.finish_closing(server, &mut events)?;
        let on_it = self.entry().is_some_and(|current| current.entry_id == from);
        if let (true, Some(index)) = (on_it, self.current) {
            let library = library.ok_or(no_library())?;
            match direction {
                Direction::Next => {
                    self.leave(server, EndReason::Next, &mut events)?;
                    if index + 1 < self.queue.len() {
                        self.go_to(
                            index + 1,
                            StartReason::UserPick,
                            server,
                            library,
                            &mut events,
                        )?;
                    } else {
                        self.status = Status::Stopped;
                        self.position_ms = 0;
                    }
                }
                Direction::Previous if self.position() > RESTART_AFTER_MS || index == 0 => {
                    self.seek_to(0, server, Some(library), &mut events)?;
                }
                Direction::Previous => {
                    self.leave(server, EndReason::Previous, &mut events)?;
                    self.go_to(
                        index - 1,
                        StartReason::UserPick,
                        server,
                        library,
                        &mut events,
                    )?;
                }
            }
            self.changed(&mut events);
        }
        self.receipt(server, request, events)
    }

    /// Reads what the renderer has played: opens the listen at the first
    /// frame heard, checkpoints it every few seconds, and at the end of a
    /// track closes it and moves on.
    pub fn tick(
        &mut self,
        server: &ServerStore,
        library: Option<&LibraryStore>,
    ) -> Result<Vec<PlayerEvent>, Error> {
        let mut events = Vec::new();
        self.finish_closing(server, &mut events)?;
        if self.loaded.is_none() {
            return Ok(events);
        }
        let polled = self.renderer.poll();
        self.account(server, &polled, &mut events)?;
        if let Some(limit) = self.setup.fail_at_position_ms
            && self.status == Status::Playing
            && self.position() >= limit
        {
            return Err(Error::FailPoint);
        }
        let mono = self.setup.timekeeper.mono_ms();
        if let Some(failed) = polled.failed {
            let heard = self.listen.is_some();
            self.leave(server, EndReason::Error, &mut events)?;
            if let Some(entry) = self.entry().cloned() {
                if !heard {
                    server.mark_failed(entry.row, "unreadable".to_owned())?;
                }
                events.push(PlayerEvent::Failed {
                    entry: entry.entry_id,
                    reason: failed,
                });
            }
            self.advance(server, library, &mut events)?;
        } else if polled.ended {
            self.leave(server, EndReason::Finished, &mut events)?;
            self.advance(server, library, &mut events)?;
        } else if self.listen.is_some()
            && mono - self.last_write_mono >= self.setup.checkpoint_every_ms
        {
            self.checkpoint(server)?;
        }
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

    /// The track in hand cannot be played on: what it played is counted
    /// and its listen ends as an error, the entry is marked with why, and
    /// the player moves on.
    fn fail_track(
        &mut self,
        server: &ServerStore,
        library: Option<&LibraryStore>,
        failure: &RenderError,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        self.leave(server, EndReason::Error, events)?;
        if let Some(entry) = self.entry().cloned() {
            server.mark_failed(entry.row, failure.reason.to_owned())?;
            if let Some(failed) = self.current.and_then(|index| self.queue.get_mut(index)) {
                failed.failed_reason = Some(failure.reason.to_owned());
            }
            events.push(PlayerEvent::Failed {
                entry: entry.entry_id,
                reason: failure.to_string(),
            });
            events.push(PlayerEvent::Queue(self.queue_revision));
        }
        self.advance(server, library, events)
    }

    /// Moves on from a track that ended: to the next entry, or to a stop at
    /// the end of the queue.
    fn advance(
        &mut self,
        server: &ServerStore,
        library: Option<&LibraryStore>,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        let next = self.current.map(|index| index + 1);
        match (next, library) {
            (Some(next), Some(library)) if next < self.queue.len() => {
                self.go_to(next, StartReason::AutoAdvance, server, library, events)?;
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

    /// Stops for a clean shutdown: the open listen ends as `shutdown` and is
    /// what a resume after the restart continues, and the position is
    /// saved. The output is let go.
    pub fn shutdown(&mut self, server: &ServerStore) -> Result<Vec<PlayerEvent>, Error> {
        let mut events = Vec::new();
        self.renderer.pause();
        self.leave(server, EndReason::Shutdown, &mut events)?;
        if self.current.is_some() {
            self.status = Status::Paused;
            server.save_place(self.entry_row(), self.position_ms, self.resume_listen)?;
        }
        Ok(events)
    }
}
