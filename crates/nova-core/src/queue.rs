//! Play queue: ordering, shuffle and repeat.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::TrackId;

/// What happens once the queue reaches its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    /// Stop at the end of the queue.
    Off,
    /// Loop the whole queue.
    All,
    /// Loop the current track.
    One,
}

impl RepeatMode {
    /// Cycles `Off -> All -> One -> Off`.
    pub fn cycle(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }

    /// Short label for UI buttons.
    pub fn label(self) -> &'static str {
        match self {
            RepeatMode::Off => "Repeat: off",
            RepeatMode::All => "Repeat: all",
            RepeatMode::One => "Repeat: one",
        }
    }
}

/// A deterministic-enough xorshift PRNG.
///
/// Shuffle only needs to be unpredictable to the listener, so we avoid pulling
/// in a full RNG crate and seed from the wall clock.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn seeded() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        // xorshift64* requires a non-zero state.
        Rng(nanos | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }
}

/// Ordered list of tracks to play, with shuffle/repeat semantics layered on top.
///
/// `items` holds the tracks in the order the user added them; `order` is a
/// permutation over `items` indices and defines playback order. `cursor` points
/// into `order`.
#[derive(Debug, Clone)]
pub struct Queue {
    items: Vec<TrackId>,
    order: Vec<usize>,
    cursor: Option<usize>,
    shuffle: bool,
    repeat: RepeatMode,
    rng: Rng,
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    /// Creates an empty, unshuffled queue.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            order: Vec::new(),
            cursor: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            rng: Rng::seeded(),
        }
    }

    /// Replaces the contents of the queue.
    pub fn set_items(&mut self, items: Vec<TrackId>) {
        self.items = items;
        self.rebuild_order();
        self.cursor = None;
    }

    /// Replaces the contents and starts at a given position in `items`.
    pub fn start_at(&mut self, items: Vec<TrackId>, item_index: usize) -> Option<TrackId> {
        self.items = items;
        self.rebuild_order();
        self.jump_to_item(item_index)
    }

    /// All queued track ids, in insertion order.
    pub fn items(&self) -> &[TrackId] {
        &self.items
    }

    /// Number of queued tracks.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Track currently under the cursor, if any.
    pub fn current(&self) -> Option<TrackId> {
        let order_index = self.cursor?;
        let item_index = *self.order.get(order_index)?;
        self.items.get(item_index).copied()
    }

    /// Index of the current track inside [`Queue::items`].
    pub fn current_item_index(&self) -> Option<usize> {
        let order_index = self.cursor?;
        self.order.get(order_index).copied()
    }

    /// Whether shuffle is enabled.
    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Enables or disables shuffle, keeping the current track selected.
    pub fn set_shuffle(&mut self, shuffle: bool) {
        if self.shuffle == shuffle {
            return;
        }
        let keep = self.current_item_index();
        self.shuffle = shuffle;
        self.rebuild_order();
        match keep {
            Some(item_index) => {
                // In shuffle mode the current track sits right after the cursor
                // position we had, so it keeps playing.
                self.jump_to_item(item_index);
            }
            None => self.cursor = None,
        }
    }

    /// Flips shuffle on or off.
    pub fn toggle_shuffle(&mut self) {
        let next = !self.shuffle;
        self.set_shuffle(next);
    }

    /// Current repeat mode.
    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    /// Sets the repeat mode.
    pub fn set_repeat(&mut self, repeat: RepeatMode) {
        self.repeat = repeat;
    }

    /// Advances the repeat mode by one step.
    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.cycle();
    }

    /// Moves to the given item (index into `items`) and returns its id.
    pub fn jump_to_item(&mut self, item_index: usize) -> Option<TrackId> {
        let order_index = self.order.iter().position(|&i| i == item_index)?;
        self.cursor = Some(order_index);
        self.current()
    }

    /// Appends a track to the end of the queue.
    pub fn push_back(&mut self, id: TrackId) {
        self.items.push(id);
        let was_empty = self.order.is_empty();
        if was_empty {
            self.order.push(0);
        } else if !self.shuffle {
            self.order.push(self.items.len() - 1);
        } else {
            // Insert at a random spot after the cursor.
            let at = self
                .cursor
                .map(|c| c + 1 + self.rng.below(self.order.len() - c))
                .unwrap_or(0);
            self.order.insert(at, self.items.len() - 1);
        }
    }

    /// Inserts a track so it plays right after the current one.
    pub fn play_next(&mut self, id: TrackId) {
        self.items.push(id);
        let new_index = self.items.len() - 1;
        let at = self.cursor.map(|c| c + 1).unwrap_or(0);
        self.order.insert(at, new_index);
    }

    /// Removes every occurrence of a track.
    pub fn remove(&mut self, id: TrackId) {
        let current_id = self.current();
        self.items.retain(|&i| i != id);
        self.rebuild_order();
        if current_id.is_some() && self.current().is_none() {
            // The removed track was playing; keep the cursor at a valid spot.
            if self.order.is_empty() {
                self.cursor = None;
            } else {
                let c = self.cursor.unwrap_or(0).min(self.order.len() - 1);
                self.cursor = Some(c);
            }
        }
    }

    /// Empties the queue.
    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.cursor = None;
    }

    /// Advances to the next track honouring the repeat mode.
    ///
    /// Returns `None` when playback should stop.
    pub fn advance(&mut self) -> Option<TrackId> {
        if self.order.is_empty() {
            return None;
        }
        if self.repeat == RepeatMode::One {
            return self.current().or_else(|| self.jump_to_item(0));
        }

        let next = match self.cursor {
            Some(c) => c + 1,
            None => 0,
        };
        if next < self.order.len() {
            self.cursor = Some(next);
            return self.current();
        }

        match self.repeat {
            RepeatMode::All => {
                if self.shuffle {
                    self.reshuffle();
                }
                self.cursor = Some(0);
                self.current()
            }
            _ => {
                self.cursor = Some(self.order.len() - 1);
                None
            }
        }
    }

    /// Steps back one track honouring the repeat mode.
    pub fn back(&mut self) -> Option<TrackId> {
        if self.order.is_empty() {
            return None;
        }
        match self.cursor {
            Some(c) if c > 0 => {
                self.cursor = Some(c - 1);
                self.current()
            }
            Some(_) => match self.repeat {
                RepeatMode::All => {
                    self.cursor = Some(self.order.len() - 1);
                    self.current()
                }
                _ => self.current(),
            },
            None => self.jump_to_item(0),
        }
    }

    fn rebuild_order(&mut self) {
        self.order = (0..self.items.len()).collect();
        if self.shuffle {
            self.reshuffle();
        }
    }

    fn reshuffle(&mut self) {
        // Fisher-Yates over `order`.
        for i in (1..self.order.len()).rev() {
            let j = self.rng.below(i + 1);
            self.order.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u64) -> Vec<TrackId> {
        (0..n).collect()
    }

    #[test]
    fn linear_playback_walks_insertion_order() {
        let mut q = Queue::new();
        q.set_items(ids(3));
        assert_eq!(q.advance(), Some(0));
        assert_eq!(q.advance(), Some(1));
        assert_eq!(q.advance(), Some(2));
        assert_eq!(q.advance(), None);
    }

    #[test]
    fn repeat_all_wraps_around() {
        let mut q = Queue::new();
        q.set_items(ids(2));
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.advance(), Some(0));
        assert_eq!(q.advance(), Some(1));
        assert_eq!(q.advance(), Some(0));
    }

    #[test]
    fn repeat_one_keeps_returning_current() {
        let mut q = Queue::new();
        q.set_items(ids(3));
        q.start_at(ids(3), 1);
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.advance(), Some(1));
        assert_eq!(q.advance(), Some(1));
    }

    #[test]
    fn start_at_selects_the_requested_item() {
        let mut q = Queue::new();
        q.set_items(ids(4));
        assert_eq!(q.start_at(ids(4), 2), Some(2));
        assert_eq!(q.current_item_index(), Some(2));
    }

    #[test]
    fn shuffle_plays_every_item_exactly_once() {
        let mut q = Queue::new();
        q.set_items(ids(8));
        q.set_shuffle(true);
        let mut seen = Vec::new();
        while let Some(id) = q.advance() {
            seen.push(id);
        }
        seen.sort_unstable();
        assert_eq!(seen, ids(8));
    }

    #[test]
    fn play_next_inserts_after_cursor() {
        let mut q = Queue::new();
        q.set_items(vec![1, 2, 3]);
        q.start_at(vec![1, 2, 3], 0);
        q.play_next(99);
        assert_eq!(q.advance(), Some(99));
        assert_eq!(q.advance(), Some(2));
    }

    #[test]
    fn back_from_start_returns_current_without_repeat() {
        let mut q = Queue::new();
        q.set_items(ids(3));
        q.start_at(ids(3), 0);
        assert_eq!(q.back(), Some(0));
    }
}
