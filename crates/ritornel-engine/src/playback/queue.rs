//! The queue in memory, in queue order and in play order, and the writes
//! that change it. See "Queue and session" in docs/internals/playback.md.
//!
//! Each entry has a position in queue order and, while shuffle is on, a
//! place in the stored play order. Both are spread with room between them,
//! so an edit writes only the entries it adds or moves, until a stretch
//! has no room left and the whole order is written afresh. An edit is
//! worked out as a [`QueueWrite`], committed to `server.db`, and only then
//! applied here, so memory and the file agree.

use std::collections::{HashMap, HashSet};

use crate::playback::store::Entry;
use crate::{Entropy, Error, Id};

/// The room left between two neighbouring positions written afresh.
pub(crate) const GAP: i64 = 1 << 16;

/// A change to the queue, as `server.db` takes it and memory applies it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct QueueWrite {
    /// Every entry goes, before anything else is done.
    pub(crate) clear: bool,
    /// The entries that go, by row.
    pub(crate) remove: Vec<i64>,
    /// Entries kept, with their new position and place in play order.
    pub(crate) moved: Vec<(i64, i64, Option<i64>)>,
    /// The entries added, with their positions.
    pub(crate) insert: Vec<Entry>,
    /// Shuffle turned on or off. Off takes every place in play order away.
    pub(crate) shuffle: Option<bool>,
}

/// Repeat: what happens at the end of an entry, and of the queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Repeat {
    /// The queue plays once.
    #[default]
    Off,
    /// The queue starts again at its end, in a new order when shuffled.
    All,
    /// The entry starts again at its end, as a new listen.
    One,
}

impl Repeat {
    /// As `server.db` writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }

    /// From what `server.db` holds.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "off" => Some(Self::Off),
            "all" => Some(Self::All),
            "one" => Some(Self::One),
            _ => None,
        }
    }
}

/// Where entries are added or moved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Where {
    /// Straight after the entry at this index, in both orders. `None` is
    /// before every entry.
    After(Option<usize>),
    /// At the end of queue order, and while shuffled at random places in
    /// play order among the entries after the one at this index: those
    /// still to play. `None` is among them all.
    End(Option<usize>),
}

/// One entry of an order being worked out: one in the queue, by its index,
/// or one being added, by its index among those.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Item {
    Old(usize),
    New(usize),
}

/// The queue.
#[derive(Debug, Clone, Default)]
pub(crate) struct Queue {
    /// In queue order.
    entries: Vec<Entry>,
    /// While shuffled, the play order, as indexes into `entries`.
    order: Option<Vec<usize>>,
    /// The row the next entry added takes.
    next_row: i64,
}

impl Queue {
    /// The queue of these entries, in any order.
    pub(crate) fn new(entries: Vec<Entry>, shuffled: bool) -> Self {
        let mut queue = Self {
            entries,
            order: None,
            next_row: 1,
        };
        queue.settle(shuffled);
        queue
    }

    /// Puts the entries in queue order and works out play order again.
    fn settle(&mut self, shuffled: bool) {
        self.entries
            .sort_by_key(|entry| (entry.position, entry.row));
        let highest = self
            .entries
            .iter()
            .map(|entry| entry.row)
            .max()
            .unwrap_or(0);
        self.next_row = self.next_row.max(highest.saturating_add(1));
        self.order = shuffled.then(|| {
            let mut order: Vec<usize> = (0..self.entries.len()).collect();
            // An entry with no place yet, which only an older file can
            // hold, plays after those with one.
            order.sort_by_key(|index| {
                self.entries.get(*index).map(|entry| {
                    (
                        entry.shuffle_position.is_none(),
                        entry.shuffle_position,
                        entry.position,
                        entry.row,
                    )
                })
            });
            order
        });
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn get(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    pub(crate) fn get_mut(&mut self, index: usize) -> Option<&mut Entry> {
        self.entries.get_mut(index)
    }

    /// The entries, in queue order.
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, Entry> {
        self.entries.iter()
    }

    /// Whether it plays in its stored shuffled order.
    pub(crate) fn shuffled(&self) -> bool {
        self.order.is_some()
    }

    /// The index of the entry with this ID.
    pub(crate) fn index_of(&self, entry: Id) -> Option<usize> {
        self.entries
            .iter()
            .position(|found| found.entry_id == entry)
    }

    /// The index of the entry in this row.
    pub(crate) fn index_of_row(&self, row: i64) -> Option<usize> {
        self.entries.iter().position(|found| found.row == row)
    }

    /// The index of the entry `slot` places on in play order.
    pub(crate) fn at(&self, slot: usize) -> Option<usize> {
        match &self.order {
            Some(order) => order.get(slot).copied(),
            None => (slot < self.entries.len()).then_some(slot),
        }
    }

    /// The place in play order of the entry at `index`.
    pub(crate) fn slot(&self, index: usize) -> Option<usize> {
        match &self.order {
            Some(order) => order.iter().position(|found| *found == index),
            None => (index < self.entries.len()).then_some(index),
        }
    }

    /// The entry that plays after the one at `index`.
    pub(crate) fn after(&self, index: usize) -> Option<usize> {
        self.at(self.slot(index)?.checked_add(1)?)
    }

    /// The entry that plays before the one at `index`.
    pub(crate) fn before(&self, index: usize) -> Option<usize> {
        self.at(self.slot(index)?.checked_sub(1)?)
    }

    /// The entry that plays first.
    pub(crate) fn first(&self) -> Option<usize> {
        self.at(0)
    }

    /// The entry that plays last.
    pub(crate) fn last(&self) -> Option<usize> {
        self.at(self.entries.len().checked_sub(1)?)
    }

    /// The entries from place `from` on in play order, at most `limit`.
    pub(crate) fn play_window(&self, from: usize, limit: usize) -> Vec<usize> {
        (from..self.entries.len())
            .take(limit)
            .filter_map(|slot| self.at(slot))
            .collect()
    }

    /// Entries for these tracks, with rows and IDs of their own and no
    /// positions yet.
    pub(crate) fn fresh(&self, tracks: Vec<(Id, Id)>, template: &Entry) -> Vec<Entry> {
        tracks
            .into_iter()
            .zip(self.next_row..)
            .map(|((entry_id, appearance), row)| Entry {
                row,
                entry_id,
                appearance,
                failed_reason: None,
                ..template.clone()
            })
            .collect()
    }

    /// Adds entries at `at`.
    pub(crate) fn add(
        &self,
        added: Vec<Entry>,
        at: Where,
        random: &mut Random<'_>,
    ) -> Result<QueueWrite, Error> {
        let news: Vec<Item> = (0..added.len()).map(Item::New).collect();
        let olds: Vec<Item> = (0..self.entries.len()).map(Item::Old).collect();
        let queue_order = match at {
            Where::After(anchor) => spliced(&olds, anchor.map_or(0, |index| index + 1), &news),
            Where::End(_) => spliced(&olds, olds.len(), &news),
        };
        let play_order = match &self.order {
            None => None,
            Some(order) => {
                let olds: Vec<Item> = order.iter().copied().map(Item::Old).collect();
                let cut = |anchor: Option<usize>| {
                    anchor
                        .and_then(|index| self.slot(index))
                        .map_or(0, |slot| slot + 1)
                };
                Some(match at {
                    Where::After(anchor) => spliced(&olds, cut(anchor), &news),
                    Where::End(after) => scattered(&olds, cut(after), news.clone(), random)?,
                })
            }
        };
        Ok(self.plan(added, &HashSet::new(), &queue_order, play_order.as_deref()))
    }

    /// Moves the entries at `moving`, in that order, to straight after the
    /// entry at `anchor` in both orders, or before every entry with `None`.
    /// The anchor is not one of them.
    pub(crate) fn move_after(&self, moving: &[usize], anchor: Option<usize>) -> QueueWrite {
        self.moved(moving, |kept| {
            anchor
                .and_then(|anchor| kept.iter().position(|item| *item == Item::Old(anchor)))
                .map_or(0, |at| at + 1)
        })
    }

    /// Moves the entries at `moving`, in that order, to the end of both
    /// orders.
    pub(crate) fn move_to_end(&self, moving: &[usize]) -> QueueWrite {
        self.moved(moving, <[Item]>::len)
    }

    /// Moves the entries at `moving` to where `cut` says among the rest, in
    /// each order.
    fn moved(&self, moving: &[usize], cut: impl Fn(&[Item]) -> usize) -> QueueWrite {
        let loose: HashSet<usize> = moving.iter().copied().collect();
        let moved: Vec<Item> = moving.iter().copied().map(Item::Old).collect();
        let place = |order: Vec<usize>| {
            let kept: Vec<Item> = order
                .into_iter()
                .filter(|index| !loose.contains(index))
                .map(Item::Old)
                .collect();
            spliced(&kept, cut(&kept), &moved)
        };
        let queue_order = place((0..self.entries.len()).collect());
        let play_order = self.order.clone().map(place);
        self.plan(Vec::new(), &loose, &queue_order, play_order.as_deref())
    }

    /// Takes out the entries at these indexes.
    pub(crate) fn remove(&self, indexes: &[usize]) -> QueueWrite {
        QueueWrite {
            remove: indexes
                .iter()
                .filter_map(|index| self.entries.get(*index))
                .map(|entry| entry.row)
                .collect(),
            ..QueueWrite::default()
        }
    }

    /// Turns shuffle on: a new random play order, with the entry at `first`
    /// first.
    pub(crate) fn shuffle(
        &self,
        first: Option<usize>,
        random: &mut Random<'_>,
    ) -> Result<QueueWrite, Error> {
        let mut rest: Vec<usize> = (0..self.entries.len())
            .filter(|index| Some(*index) != first)
            .collect();
        random.shuffle(&mut rest)?;
        let order: Vec<Item> = first.into_iter().chain(rest).map(Item::Old).collect();
        let places: HashMap<Item, i64> = afresh(&order).into_iter().collect();
        Ok(QueueWrite {
            moved: self
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    (
                        entry.row,
                        entry.position,
                        places.get(&Item::Old(index)).copied(),
                    )
                })
                .collect(),
            shuffle: Some(true),
            ..QueueWrite::default()
        })
    }

    /// Turns shuffle off: queue order again.
    pub(crate) fn unshuffle() -> QueueWrite {
        QueueWrite {
            shuffle: Some(false),
            ..QueueWrite::default()
        }
    }

    /// Puts these entries in place of every entry, shuffled with the one
    /// at `first` first, or in the order given.
    pub(crate) fn replace(
        &self,
        mut added: Vec<Entry>,
        first: Option<usize>,
        shuffled: bool,
        random: &mut Random<'_>,
    ) -> Result<QueueWrite, Error> {
        for (entry, position) in added.iter_mut().zip(positions()) {
            entry.position = position;
            entry.shuffle_position = None;
        }
        if shuffled {
            let mut rest: Vec<usize> = (0..added.len())
                .filter(|index| Some(*index) != first)
                .collect();
            random.shuffle(&mut rest)?;
            for (index, place) in first.into_iter().chain(rest).zip(positions()) {
                if let Some(entry) = added.get_mut(index) {
                    entry.shuffle_position = Some(place);
                }
            }
        }
        Ok(QueueWrite {
            clear: true,
            insert: added,
            shuffle: Some(shuffled),
            ..QueueWrite::default()
        })
    }

    /// The write that puts the entries in these orders: each keeps the
    /// position it has where it can, and the added and the loose are given
    /// theirs.
    fn plan(
        &self,
        mut added: Vec<Entry>,
        loose: &HashSet<usize>,
        queue_order: &[Item],
        play_order: Option<&[Item]>,
    ) -> QueueWrite {
        let positions: HashMap<Item, i64> = spread(queue_order, |item| match item {
            Item::Old(index) if !loose.contains(&index) => {
                self.entries.get(index).map(|entry| entry.position)
            }
            _ => None,
        })
        .into_iter()
        .collect();
        let places: HashMap<Item, i64> = play_order
            .map(|order| {
                spread(order, |item| match item {
                    Item::Old(index) if !loose.contains(&index) => self
                        .entries
                        .get(index)
                        .and_then(|entry| entry.shuffle_position),
                    _ => None,
                })
            })
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut write = QueueWrite::default();
        for (index, entry) in self.entries.iter().enumerate() {
            let item = Item::Old(index);
            let position = positions.get(&item).copied();
            let place = places.get(&item).copied();
            if position.is_some() || place.is_some() {
                write.moved.push((
                    entry.row,
                    position.unwrap_or(entry.position),
                    place.or(entry.shuffle_position),
                ));
            }
        }
        for (index, entry) in added.iter_mut().enumerate() {
            let item = Item::New(index);
            entry.position = positions.get(&item).copied().unwrap_or_default();
            entry.shuffle_position = places.get(&item).copied();
        }
        write.insert = added;
        write
    }

    /// Applies a write that has been committed.
    pub(crate) fn apply(&mut self, write: &QueueWrite) {
        if write.clear {
            self.entries.clear();
        }
        if !write.remove.is_empty() {
            let gone: HashSet<i64> = write.remove.iter().copied().collect();
            self.entries.retain(|entry| !gone.contains(&entry.row));
        }
        if !write.moved.is_empty() {
            let moved: HashMap<i64, (i64, Option<i64>)> = write
                .moved
                .iter()
                .map(|(row, position, place)| (*row, (*position, *place)))
                .collect();
            for entry in &mut self.entries {
                if let Some((position, place)) = moved.get(&entry.row) {
                    entry.position = *position;
                    entry.shuffle_position = *place;
                }
            }
        }
        if write.shuffle == Some(false) {
            for entry in &mut self.entries {
                entry.shuffle_position = None;
            }
        }
        self.entries.extend(write.insert.iter().cloned());
        let shuffled = write.shuffle.unwrap_or(self.order.is_some());
        self.settle(shuffled);
    }
}

/// `items` with `put` in at `at`.
fn spliced(items: &[Item], at: usize, put: &[Item]) -> Vec<Item> {
    let at = at.min(items.len());
    let (head, tail) = items.split_at(at);
    head.iter().chain(put).chain(tail).copied().collect()
}

/// `olds` with each of `news` at a random place from `from` on, the new
/// ones in a random order among themselves.
fn scattered(
    olds: &[Item],
    from: usize,
    mut news: Vec<Item>,
    random: &mut Random<'_>,
) -> Result<Vec<Item>, Error> {
    random.shuffle(&mut news)?;
    let from = from.min(olds.len());
    let gaps = olds.len() - from + 1;
    let mut placed: Vec<(usize, Item)> = Vec::with_capacity(news.len());
    for item in news {
        placed.push((from + random.below(gaps)?, item));
    }
    // A stable sort keeps the random order of those sharing a gap.
    placed.sort_by_key(|(gap, _)| *gap);
    let mut order = Vec::with_capacity(olds.len() + placed.len());
    let mut placed = placed.into_iter().peekable();
    for (at, old) in olds.iter().enumerate() {
        while let Some((_, item)) = placed.next_if(|(gap, _)| *gap == at) {
            order.push(item);
        }
        order.push(*old);
    }
    order.extend(placed.map(|(_, item)| item));
    Ok(order)
}

/// Positions written afresh: 0, then a gap apart.
fn positions() -> impl Iterator<Item = i64> {
    (0_i64..).map(|at| at.saturating_mul(GAP))
}

/// Every item of `order` given a position afresh.
fn afresh(order: &[Item]) -> Vec<(Item, i64)> {
    order.iter().copied().zip(positions()).collect()
}

/// Positions for the items of `order` that `have` gives none, between the
/// neighbours that keep theirs; or, where some stretch has no room, a
/// position afresh for every item.
fn spread(order: &[Item], have: impl Fn(Item) -> Option<i64>) -> Vec<(Item, i64)> {
    let mut given = Vec::new();
    let mut before = None;
    let mut at = 0;
    while let Some(item) = order.get(at).copied() {
        if let Some(position) = have(item) {
            before = Some(position);
            at += 1;
            continue;
        }
        let start = at;
        while order.get(at).is_some_and(|item| have(*item).is_none()) {
            at += 1;
        }
        let after = order.get(at).and_then(|item| have(*item));
        let run = order.get(start..at).unwrap_or_default();
        match between(before, after, run.len()) {
            Some(positions) => given.extend(run.iter().copied().zip(positions)),
            None => return afresh(order),
        }
    }
    given
}

/// `count` positions in order, after `before` and before `after`, or `None`
/// where there is not room for them.
fn between(before: Option<i64>, after: Option<i64>, count: usize) -> Option<Vec<i64>> {
    let count = i64::try_from(count).ok()?;
    let steps = 1..=count;
    match (before, after) {
        (None, None) => steps.map(|at| (at - 1).checked_mul(GAP)).collect(),
        (Some(before), None) => steps
            .map(|at| at.checked_mul(GAP).and_then(|gap| before.checked_add(gap)))
            .collect(),
        (None, Some(after)) => steps
            .map(|at| {
                (count - at + 1)
                    .checked_mul(GAP)
                    .and_then(|gap| after.checked_sub(gap))
            })
            .collect(),
        (Some(before), Some(after)) => {
            let step = after.checked_sub(before)? / count.checked_add(1)?;
            (step >= 1).then(|| steps.map(|at| before + at * step).collect())
        }
    }
}

/// Random numbers, drawn from the store's entropy a batch at a time.
pub(crate) struct Random<'a> {
    entropy: &'a dyn Entropy,
    pool: Vec<u8>,
    used: usize,
}

impl<'a> Random<'a> {
    pub(crate) fn new(entropy: &'a dyn Entropy) -> Self {
        Self {
            entropy,
            pool: Vec::new(),
            used: 0,
        }
    }

    /// A number from 0 to below `bound`, which is more than 0.
    pub(crate) fn below(&mut self, bound: usize) -> Result<usize, Error> {
        if self.used + 8 > self.pool.len() {
            self.pool = vec![0; 4096];
            self.entropy.fill(&mut self.pool)?;
            self.used = 0;
        }
        let mut bytes = [0; 8];
        if let Some(drawn) = self.pool.get(self.used..self.used + 8) {
            bytes.copy_from_slice(drawn);
        }
        self.used += 8;
        let bound = u64::try_from(bound.max(1)).unwrap_or(u64::MAX);
        Ok(usize::try_from(u64::from_le_bytes(bytes) % bound).unwrap_or(0))
    }

    /// Puts `items` in a random order.
    pub(crate) fn shuffle<T>(&mut self, items: &mut [T]) -> Result<(), Error> {
        for at in (1..items.len()).rev() {
            let other = self.below(at + 1)?;
            items.swap(at, other);
        }
        Ok(())
    }
}

impl std::fmt::Debug for Random<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Random").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use ritornel_domain::history::ContextKind;

    use super::*;
    use crate::OsEntropy;

    fn entry(row: i64, position: i64) -> Entry {
        Entry {
            row,
            entry_id: Id::from_bytes([u8::try_from(row).unwrap(); 16]),
            appearance: Id::from_bytes([0xa0; 16]),
            position,
            shuffle_position: None,
            context_kind: ContextKind::Album,
            context_ref: None,
            added_by: None,
            failed_reason: None,
        }
    }

    /// A queue of `count` entries, rows from 1, written afresh.
    fn queue(count: i64) -> Queue {
        Queue::new(
            (1..=count).map(|row| entry(row, (row - 1) * GAP)).collect(),
            false,
        )
    }

    fn rows(queue: &Queue) -> Vec<i64> {
        queue.iter().map(|entry| entry.row).collect()
    }

    fn play_rows(queue: &Queue) -> Vec<i64> {
        (0..queue.len())
            .filter_map(|slot| queue.at(slot))
            .map(|index| queue.get(index).unwrap().row)
            .collect()
    }

    fn added(queue: &Queue, count: usize) -> Vec<Entry> {
        let tracks = (0..count)
            .map(|at| {
                let byte = 0x80 + u8::try_from(at).unwrap();
                (Id::from_bytes([byte; 16]), Id::from_bytes([0xa1; 16]))
            })
            .collect();
        queue.fresh(tracks, &entry(0, 0))
    }

    #[test]
    fn adding_after_an_entry_writes_only_what_is_added() {
        let mut queue = queue(4);
        let mut random = Random::new(&OsEntropy);
        let write = queue
            .add(added(&queue, 2), Where::After(Some(1)), &mut random)
            .unwrap();
        assert!(write.moved.is_empty());
        assert_eq!(write.insert.len(), 2);
        queue.apply(&write);
        assert_eq!(rows(&queue), [1, 2, 5, 6, 3, 4]);
        // At the front and at the end there is always room.
        let write = queue
            .add(added(&queue, 1), Where::After(None), &mut random)
            .unwrap();
        assert!(write.moved.is_empty());
        queue.apply(&write);
        let write = queue
            .add(added(&queue, 1), Where::End(None), &mut random)
            .unwrap();
        queue.apply(&write);
        assert_eq!(rows(&queue), [7, 1, 2, 5, 6, 3, 4, 8]);
    }

    #[test]
    fn a_stretch_with_no_room_left_is_written_afresh() {
        let mut queue = Queue::new(vec![entry(1, 0), entry(2, 1)], false);
        let mut random = Random::new(&OsEntropy);
        let write = queue
            .add(added(&queue, 1), Where::After(Some(0)), &mut random)
            .unwrap();
        assert_eq!(write.moved, [(1, 0, None), (2, 2 * GAP, None)]);
        queue.apply(&write);
        assert_eq!(rows(&queue), [1, 3, 2]);
        let positions: Vec<i64> = queue.iter().map(|entry| entry.position).collect();
        assert_eq!(positions, [0, GAP, 2 * GAP]);
    }

    #[test]
    fn shuffle_puts_the_current_entry_first_and_every_entry_once() {
        let mut queue = queue(50);
        let mut random = Random::new(&OsEntropy);
        let write = queue.shuffle(Some(9), &mut random).unwrap();
        queue.apply(&write);
        assert!(queue.shuffled());
        let order = play_rows(&queue);
        assert_eq!(order[0], 10);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (1..=50).collect::<Vec<_>>());
        assert_ne!(order, sorted, "fifty entries are not left in order");
        // Queue order is as it was, and off goes back to it.
        assert_eq!(rows(&queue), sorted);
        queue.apply(&Queue::unshuffle());
        assert!(!queue.shuffled());
        assert_eq!(play_rows(&queue), sorted);
        assert!(queue.iter().all(|entry| entry.shuffle_position.is_none()));
    }

    #[test]
    fn while_shuffled_next_goes_after_the_current_entry_and_the_end_among_those_still_to_play() {
        let mut queue = queue(20);
        let mut random = Random::new(&OsEntropy);
        queue.apply(&queue.shuffle(Some(0), &mut random).unwrap());
        // Five entries in, the current one is the fifth in play order.
        let current_row = queue.get(queue.at(4).unwrap()).unwrap().row;
        let current = |queue: &Queue| queue.index_of_row(current_row).unwrap();
        let played: Vec<i64> = play_rows(&queue)[..5].to_vec();

        let write = queue
            .add(
                added(&queue, 1),
                Where::After(Some(current(&queue))),
                &mut random,
            )
            .unwrap();
        queue.apply(&write);
        let next = queue.after(current(&queue)).unwrap();
        assert_eq!(queue.get(next).unwrap().row, 21);
        // In queue order it is straight after the current entry too.
        let at = queue.index_of_row(21).unwrap();
        assert_eq!(queue.get(at - 1).unwrap().row, current_row);

        let current = current(&queue);
        let write = queue
            .add(added(&queue, 10), Where::End(Some(current)), &mut random)
            .unwrap();
        queue.apply(&write);
        let order = play_rows(&queue);
        assert_eq!(
            &order[..5],
            &played[..],
            "what was played stays where it was"
        );
        let tail = &order[5..];
        assert!(tail.contains(&22) && tail.contains(&31));
        // In queue order they are at the end.
        assert_eq!(rows(&queue)[21..], (22..=31).collect::<Vec<_>>()[..]);
    }

    #[test]
    fn moved_entries_go_straight_after_the_anchor_in_both_orders() {
        let mut queue = queue(6);
        let write = queue.move_after(&[4, 5], Some(0));
        queue.apply(&write);
        assert_eq!(rows(&queue), [1, 5, 6, 2, 3, 4]);
        let write = queue.move_after(&[0], None);
        assert!(write.moved.len() == 1);
        queue.apply(&write);
        assert_eq!(rows(&queue), [1, 5, 6, 2, 3, 4]);

        let mut random = Random::new(&OsEntropy);
        queue.apply(&queue.shuffle(Some(0), &mut random).unwrap());
        let anchor = queue.at(2).unwrap();
        let moving = queue.at(5).unwrap();
        let anchor_row = queue.get(anchor).unwrap().row;
        let moved_row = queue.get(moving).unwrap().row;
        queue.apply(&queue.move_after(&[moving], Some(anchor)));
        let order = play_rows(&queue);
        let at = order.iter().position(|row| *row == anchor_row).unwrap();
        assert_eq!(order[at + 1], moved_row);
    }

    #[test]
    fn replacing_writes_every_entry_afresh_in_order_or_shuffled_with_the_first_first() {
        let queue = queue(3);
        let mut random = Random::new(&OsEntropy);
        let mut fresh = queue.clone();
        let write = queue
            .replace(added(&queue, 8), Some(3), true, &mut random)
            .unwrap();
        assert!(write.clear);
        fresh.apply(&write);
        assert_eq!(fresh.len(), 8);
        assert_eq!(play_rows(&fresh)[0], 7);
        assert_eq!(rows(&fresh), (4..=11).collect::<Vec<_>>());
    }
}
