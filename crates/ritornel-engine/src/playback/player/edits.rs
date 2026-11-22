//! What changes the queue and the player's settings: shuffle and repeat,
//! playing an entry now, and adding, taking out, moving and clearing
//! entries by their IDs. See "The queue" in docs/design/playback.md.
//!
//! An edit may name the queue revision it was decided on. If the queue has
//! moved past it, the edit changes nothing and fails with
//! [`Error::StaleQueue`]; the player is the queue's one writer, so its own
//! count is the one checked, and the store checks again as it writes.

use std::collections::HashSet;

use ritornel_domain::history::{ContextKind, EndReason, StartReason};

use super::{Done, Player, PlayerEvent, Request, Status, Summary, no_library};
use crate::id::new_ids;
use crate::playback::queue::{Queue, QueueWrite, Random, Repeat, Where};
use crate::playback::store::{Change, Entry, Wrote};
use crate::{Error, Id, LibraryStore, ServerStore};

/// Where `add` puts what it adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Straight after the current entry, in queue order and in play order.
    Next,
    /// At the end of the queue, and while shuffled at a random place among
    /// the entries still to play.
    End,
    /// Straight after this entry, in both orders.
    After(Id),
}

/// What to add to the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddTracks {
    /// The track appearances, in the order they go in.
    pub tracks: Vec<Id>,
    /// Where.
    pub place: Place,
    /// Why they were queued, and what from.
    pub context: (ContextKind, Option<Id>),
    /// The session that queued them.
    pub actor: Option<Id>,
    /// Whether to play what plays first of them, if the queue was empty.
    /// The queue is read as the edit is made, so two adds at once cannot
    /// both find it empty.
    pub play_if_empty: bool,
}

/// The player's settings, each an absolute value; one left out stays as
/// it is.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Settings {
    /// The player's own volume, 0 to 1.
    pub volume: Option<f64>,
    /// Whether it is muted.
    pub muted: Option<bool>,
    /// Shuffle.
    pub shuffle: Option<bool>,
    /// Repeat.
    pub repeat: Option<Repeat>,
}

/// Which order a window of the queue is read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// The order entries were put in.
    Queue,
    /// The order they play in.
    Play,
    /// What plays after the current entry, in play order.
    UpNext,
}

/// A window of the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePage {
    /// Each entry: its ID, its track appearance, and why it could not be
    /// played, once it could not.
    pub entries: Vec<(Id, Id, Option<String>)>,
    /// How many entries there are in the order read.
    pub total: usize,
    /// Where the current entry is in that order.
    pub current: Option<usize>,
}

impl Player {
    /// Entries for these tracks, queued for `context` by `actor`, with
    /// rows and IDs of their own.
    pub(super) fn fresh(
        &self,
        server: &ServerStore,
        tracks: Vec<Id>,
        context: (ContextKind, Option<Id>),
        actor: Option<Id>,
    ) -> Result<Vec<Entry>, Error> {
        let added_by = match actor {
            Some(actor) => server.session_row(actor)?,
            None => None,
        };
        let ids = new_ids(
            self.setup.clock.now_ms(),
            tracks.len(),
            &*self.setup.entropy,
        )?;
        let template = Entry {
            row: 0,
            entry_id: Id::from_bytes([0; 16]),
            appearance: Id::from_bytes([0; 16]),
            position: 0,
            shuffle_position: None,
            context_kind: context.0,
            context_ref: context.1,
            added_by,
            failed_reason: None,
        };
        Ok(self
            .queue
            .fresh(ids.into_iter().zip(tracks).collect(), &template))
    }

    /// Writes `change` with the request's receipt and applies it to the
    /// queue here. The player is then on the entry `change` places it on.
    /// `entry` is the entry the command's answer names.
    pub(super) fn commit(
        &mut self,
        server: &ServerStore,
        request: Option<Request>,
        mut change: Change,
        entry: Option<Id>,
    ) -> Result<Wrote, Error> {
        let answer: Box<dyn FnOnce(Summary) -> String + Send> = match request {
            Some(request) => {
                change.command = Some((request.command, request.method));
                request.answer
            }
            None => Box::new(|_| String::new()),
        };
        let row = change.place.0;
        let follows = change.bump || change.repeat.is_some();
        let wrote = server.write_queue(change, move |revision| {
            answer(Summary {
                queue_revision: revision,
                entry,
            })
        })?;
        if let Wrote::Done {
            revision, queue, ..
        } = &wrote
        {
            self.queue.apply(queue);
            self.queue_revision = *revision;
            self.current = row.and_then(|row| self.queue.index_of_row(row));
            // What follows may have changed: one prepared for a join goes.
            if follows {
                self.forget_prepared();
            }
        }
        Ok(wrote)
    }

    /// Refuses an edit decided on a queue revision this queue has moved
    /// past.
    fn fresh_enough(&self, expected: Option<i64>) -> Result<(), Error> {
        match expected {
            Some(expected) if expected != self.queue_revision => Err(Error::StaleQueue {
                current: self.queue_revision,
            }),
            _ => Ok(()),
        }
    }

    /// Where the player is now, as a change saves it.
    fn here(&self) -> (Option<i64>, i64, Option<Id>) {
        (self.entry_row(), self.position(), self.resume_listen)
    }

    /// The answer of a change, or the command's stored answer if it was
    /// carried out before.
    fn answered(wrote: Wrote, events: &[PlayerEvent]) -> Result<String, Done> {
        match wrote {
            Wrote::Done { answer, .. } => Ok(answer),
            Wrote::Replayed(answer) => Err(Done {
                answer,
                replayed: true,
                events: events.to_vec(),
            }),
        }
    }

    /// Starts the queue again from the entry that plays first, in a new
    /// order when it is shuffled, which the entry that just ended does not
    /// open where there is another.
    pub(super) fn wrap(
        &mut self,
        reason: StartReason,
        server: &ServerStore,
        library: &LibraryStore,
        events: &mut Vec<PlayerEvent>,
    ) -> Result<(), Error> {
        if self.queue.shuffled() && self.queue.len() > 1 {
            let mut random = Random::new(&*self.setup.entropy);
            let mut first = random.below(self.queue.len())?;
            if Some(first) == self.current {
                first = (first + 1) % self.queue.len();
            }
            // The new order and the entry that opens it are saved together.
            let opens = self.queue.get(first).map(|entry| entry.row);
            let change = Change {
                command: None,
                expected: None,
                queue: self.queue.shuffle(Some(first), &mut random)?,
                bump: true,
                repeat: None,
                volume: None,
                place: (opens, 0, None),
            };
            self.commit(server, None, change, None)?;
            events.push(PlayerEvent::Queue(self.queue_revision));
        }
        match self.queue.first() {
            Some(first) => self.go_to(first, reason, server, library, events),
            None => {
                self.status = Status::Stopped;
                Ok(())
            }
        }
    }

    /// A window of the queue, in `order`.
    pub fn window(&self, order: Order, offset: usize, limit: usize) -> QueuePage {
        let length = self.queue.len();
        let (indexes, total, current) = match order {
            Order::Queue => ((offset..length).take(limit).collect(), length, self.current),
            Order::Play => (
                self.queue.play_window(offset, limit),
                length,
                self.current.and_then(|index| self.queue.slot(index)),
            ),
            Order::UpNext => {
                let from = self
                    .current
                    .and_then(|index| self.queue.slot(index))
                    .map_or(0, |slot| slot + 1);
                (
                    self.queue.play_window(from.saturating_add(offset), limit),
                    length.saturating_sub(from),
                    None,
                )
            }
        };
        QueuePage {
            entries: indexes
                .into_iter()
                .filter_map(|index| self.queue.get(index))
                .map(|entry| {
                    (
                        entry.entry_id,
                        entry.appearance,
                        entry.failed_reason.clone(),
                    )
                })
                .collect(),
            total,
            current,
        }
    }

    /// Reads again what the listener has said of the track the player is
    /// on, after a rating was set. Whether it changed.
    pub fn said_again(&mut self, library: &LibraryStore) -> Result<bool, Error> {
        let Some(display) = &mut self.display else {
            return Ok(false);
        };
        let before = (display.rating, display.loved);
        library.said_again(display)?;
        let changed = before != (display.rating, display.loved);
        if changed {
            self.revision += 1;
        }
        Ok(changed)
    }

    /// Sets the volume, whether it is muted, shuffle and repeat, in one
    /// transaction. Shuffle on writes a new random play order with the
    /// current entry first; off goes back to queue order at the current
    /// entry. See "The pipeline" and "The queue" in docs/design/playback.md.
    pub fn set(
        &mut self,
        request: Request,
        settings: Settings,
        server: &ServerStore,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let Settings {
            volume,
            muted,
            shuffle,
            repeat,
        } = settings;
        if volume.is_some_and(|volume| !(0.0..=1.0).contains(&volume)) {
            return Err(Error::Invalid("the volume is from 0 to 1"));
        }
        let gain = (volume.is_some() || muted.is_some())
            .then(|| (volume.unwrap_or(self.volume), muted.unwrap_or(self.muted)));
        let mut events = Vec::new();
        self.sync(server, &mut events);
        let mut random = Random::new(&*self.setup.entropy);
        let write = match shuffle {
            Some(true) if !self.queue.shuffled() => {
                Some(self.queue.shuffle(self.current, &mut random)?)
            }
            Some(false) if self.queue.shuffled() => Some(Queue::unshuffle()),
            _ => None,
        };
        let reordered = write.is_some();
        let change = Change {
            command: None,
            expected: None,
            queue: write.unwrap_or_default(),
            bump: reordered,
            repeat,
            volume: gain,
            place: self.here(),
        };
        let entry = self.entry().map(|entry| entry.entry_id);
        if let Some(repeat) = repeat {
            self.repeat = repeat;
        }
        let wrote = self.commit(server, Some(request), change, entry);
        let answer = match wrote.map(|wrote| Self::answered(wrote, &events)) {
            Ok(Ok(answer)) => answer,
            Ok(Err(done)) => return Ok(done),
            Err(error) => {
                self.repeat = server.playback_session()?.repeat;
                return Err(error);
            }
        };
        if let Some((volume, muted)) = gain {
            (self.volume, self.muted) = (volume, muted);
            self.renderer
                .set_volume(if muted { 0.0 } else { volume as f32 });
        }
        if reordered {
            events.push(PlayerEvent::Queue(self.queue_revision));
        }
        self.changed(&mut events);
        Ok(Done {
            answer,
            replayed: false,
            events,
        })
    }

    /// Plays the entry named now, from its beginning. A listen that was
    /// open ends as `jump`.
    pub fn jump(
        &mut self,
        request: Request,
        entry: Id,
        server: &ServerStore,
        library: Option<&LibraryStore>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        let library = library.ok_or_else(no_library)?;
        let mut events = Vec::new();
        self.sync(server, &mut events);
        let index = self
            .queue
            .index_of(entry)
            .ok_or(Error::NotFound("no such entry in the queue"))?;
        self.leave(server, EndReason::Jump, &mut events);
        self.current = Some(index);
        self.position_ms = 0;
        self.resume_listen = None;
        self.start_reason = StartReason::UserPick;
        self.failures_in_a_row = 0;
        self.start(server, library, &mut events)?;
        self.changed(&mut events);
        self.receipt(server, request, events)
    }

    /// Adds tracks to the queue. With nothing current, the player waits,
    /// stopped, on the entry that plays first, or plays it if the queue was
    /// empty and `play_if_empty` asks.
    pub fn add(
        &mut self,
        request: Request,
        expected: Option<i64>,
        what: AddTracks,
        server: &ServerStore,
        library: Option<&LibraryStore>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        self.fresh_enough(expected)?;
        let library = library.ok_or_else(no_library)?;
        if what.tracks.is_empty() {
            return Err(Error::Invalid("there is nothing to add"));
        }
        if library.unknown_appearance(&what.tracks)?.is_some() {
            return Err(Error::NotFound("no such track"));
        }
        let mut events = Vec::new();
        self.sync(server, &mut events);
        let plays = what.play_if_empty && self.queue.len() == 0;
        if plays {
            self.find_listener(server, library)?;
        }
        let at = match what.place {
            Place::Next => Where::After(self.current),
            Place::End => Where::End(self.current),
            Place::After(entry) => Where::After(Some(
                self.queue
                    .index_of(entry)
                    .ok_or(Error::NotFound("no such entry in the queue"))?,
            )),
        };
        let added = self.fresh(server, what.tracks, what.context, what.actor)?;
        let mut random = Random::new(&*self.setup.entropy);
        let write = self.queue.add(added, at, &mut random)?;
        // With nothing on, the player waits on what plays first once they
        // are in: chosen here, to be saved with them and the receipt.
        let waits = self.current.is_none();
        let (place, entry) = if waits {
            let mut after = self.queue.clone();
            after.apply(&write);
            let first = after.first().and_then(|index| after.get(index));
            (
                (first.map(|entry| entry.row), 0, None),
                first.map(|entry| entry.entry_id),
            )
        } else {
            (self.here(), self.entry().map(|entry| entry.entry_id))
        };
        let change = Change {
            command: None,
            expected,
            queue: write,
            bump: true,
            repeat: None,
            volume: None,
            place,
        };
        let wrote = self.commit(server, Some(request), change, entry)?;
        let answer = match Self::answered(wrote, &events) {
            Ok(answer) => answer,
            Err(done) => return Ok(done),
        };
        if waits {
            self.position_ms = 0;
            self.resume_listen = None;
            self.display = self.read_display(Some(library));
        }
        events.push(PlayerEvent::Queue(self.queue_revision));
        if plays {
            self.start_reason = StartReason::UserPick;
            self.failures_in_a_row = 0;
            self.start(server, library, &mut events)?;
        }
        self.changed(&mut events);
        Ok(Done {
            answer,
            replayed: false,
            events,
        })
    }

    /// Takes entries out of the queue. One it no longer has is passed
    /// over. Taking out the current entry ends its listen as `removed` and
    /// moves to the next in play order that stays, playing it if the
    /// player was playing.
    pub fn remove(
        &mut self,
        request: Request,
        expected: Option<i64>,
        entries: &[Id],
        server: &ServerStore,
        library: Option<&LibraryStore>,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        self.fresh_enough(expected)?;
        let mut events = Vec::new();
        self.sync(server, &mut events);
        let gone: HashSet<usize> = entries
            .iter()
            .filter_map(|entry| self.queue.index_of(*entry))
            .collect();
        let on_it = self.current.is_some_and(|current| gone.contains(&current));
        let next = match self.current {
            Some(current) if on_it => {
                let from = self.queue.slot(current).map_or(0, |slot| slot + 1);
                (from..self.queue.len())
                    .filter_map(|slot| self.queue.at(slot))
                    .find(|index| !gone.contains(index))
            }
            current => current,
        };
        let next = next.and_then(|index| self.queue.get(index));
        let (next_row, next_id) = (
            next.map(|entry| entry.row),
            next.map(|entry| entry.entry_id),
        );
        if on_it {
            self.leave(server, EndReason::Removed, &mut events);
        }
        let indexes: Vec<usize> = gone.into_iter().collect();
        let change = Change {
            command: None,
            expected,
            queue: self.queue.remove(&indexes),
            bump: !indexes.is_empty(),
            repeat: None,
            volume: None,
            place: if on_it {
                (next_row, 0, None)
            } else {
                self.here()
            },
        };
        let wrote = self.commit(server, Some(request), change, next_id)?;
        let answer = match Self::answered(wrote, &events) {
            Ok(answer) => answer,
            Err(done) => return Ok(done),
        };
        if on_it {
            self.position_ms = 0;
            self.resume_listen = None;
            self.start_reason = StartReason::UserPick;
            match (self.current, library) {
                (Some(_), Some(library)) if self.status == Status::Playing => {
                    self.start(server, library, &mut events)?;
                }
                (Some(_), library) => self.display = self.read_display(library),
                (None, _) => {
                    self.status = Status::Stopped;
                    self.display = None;
                }
            }
        }
        events.push(PlayerEvent::Queue(self.queue_revision));
        self.changed(&mut events);
        Ok(Done {
            answer,
            replayed: false,
            events,
        })
    }

    /// Puts entries, in the order named, straight after the current entry,
    /// straight after another, or last, in both orders.
    pub fn move_entries(
        &mut self,
        request: Request,
        expected: Option<i64>,
        entries: &[Id],
        place: Place,
        server: &ServerStore,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        self.fresh_enough(expected)?;
        let mut events = Vec::new();
        self.sync(server, &mut events);
        let mut seen = HashSet::new();
        let moving: Vec<usize> = entries
            .iter()
            .filter_map(|entry| self.queue.index_of(*entry))
            .filter(|index| seen.insert(*index))
            .collect();
        let anchor = match place {
            Place::After(after) => Some(
                self.queue
                    .index_of(after)
                    .ok_or(Error::NotFound("no such entry in the queue"))?,
            ),
            Place::Next | Place::End => self.current,
        };
        if place != Place::End && anchor.is_some_and(|anchor| seen.contains(&anchor)) {
            return Err(Error::Invalid("entries cannot go after one of themselves"));
        }
        let write = match place {
            Place::End => self.queue.move_to_end(&moving),
            Place::Next | Place::After(_) => self.queue.move_after(&moving, anchor),
        };
        let change = Change {
            command: None,
            expected,
            queue: write,
            bump: !moving.is_empty(),
            repeat: None,
            volume: None,
            place: self.here(),
        };
        let entry = self.entry().map(|entry| entry.entry_id);
        let wrote = self.commit(server, Some(request), change, entry)?;
        let answer = match Self::answered(wrote, &events) {
            Ok(answer) => answer,
            Err(done) => return Ok(done),
        };
        events.push(PlayerEvent::Queue(self.queue_revision));
        self.changed(&mut events);
        Ok(Done {
            answer,
            replayed: false,
            events,
        })
    }

    /// Empties the queue and stops: a listen that was open ends as
    /// `removed`.
    pub fn clear(
        &mut self,
        request: Request,
        expected: Option<i64>,
        server: &ServerStore,
    ) -> Result<Done, Error> {
        if let Some(done) = Self::replayed(server, &request)? {
            return Ok(done);
        }
        self.fresh_enough(expected)?;
        let mut events = Vec::new();
        self.sync(server, &mut events);
        self.leave(server, EndReason::Removed, &mut events);
        let change = Change {
            command: None,
            expected,
            queue: QueueWrite {
                clear: true,
                ..QueueWrite::default()
            },
            bump: true,
            repeat: None,
            volume: None,
            place: (None, 0, None),
        };
        let wrote = self.commit(server, Some(request), change, None)?;
        let answer = match Self::answered(wrote, &events) {
            Ok(answer) => answer,
            Err(done) => return Ok(done),
        };
        self.status = Status::Stopped;
        self.position_ms = 0;
        self.resume_listen = None;
        self.display = None;
        events.push(PlayerEvent::Queue(self.queue_revision));
        self.changed(&mut events);
        Ok(Done {
            answer,
            replayed: false,
            events,
        })
    }
}
