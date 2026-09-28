use std::collections::{HashSet, VecDeque};

use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;

use crate::protocol::ContextRef;
use crate::types::LoopMode;

/// Play order for one context plus the play-next FIFO. Pure: no I/O; the
/// engine resolves the returned ids to files.
#[derive(Debug)]
pub struct Queue {
    context: Option<ContextRef>,
    /// Context order as given (unshuffled).
    base: Vec<String>,
    /// Play order: `base`, or a permutation of it in `Shuffle`.
    order: Vec<String>,
    /// `Shuffle` only: the permutation that replaces `order` when it wraps.
    next_cycle: Vec<String>,
    current: Option<String>,
    /// Index of `current` in `order` when it was reached through the order.
    cur_idx: Option<usize>,
    /// Index in `order` of the next track on forward advance.
    next_idx: usize,
    up_next: VecDeque<String>,
    loop_mode: LoopMode,
    rng: StdRng,
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub fn new() -> Queue {
        Self::from_rng(rand::make_rng())
    }

    /// Deterministic shuffles, for tests.
    pub fn with_seed(seed: u64) -> Queue {
        Self::from_rng(StdRng::seed_from_u64(seed))
    }

    fn from_rng(rng: StdRng) -> Queue {
        Queue {
            context: None,
            base: Vec::new(),
            order: Vec::new(),
            next_cycle: Vec::new(),
            current: None,
            cur_idx: None,
            next_idx: 0,
            up_next: VecDeque::new(),
            loop_mode: LoopMode::None,
            rng,
        }
    }

    pub fn start(&mut self, context: ContextRef, order: Vec<String>, start_id: &str) {
        self.context = Some(context);
        self.base = order;
        self.current = Some(start_id.to_string());
        self.rebuild_order();
    }

    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    pub fn context(&self) -> Option<&ContextRef> {
        self.context.as_ref()
    }

    pub fn set_loop(&mut self, mode: LoopMode) {
        let was_shuffle = self.loop_mode == LoopMode::Shuffle;
        self.loop_mode = mode;
        if mode == LoopMode::Shuffle || was_shuffle {
            self.rebuild_order();
        }
    }

    pub fn loop_mode(&self) -> LoopMode {
        self.loop_mode
    }

    /// What plays after the current track ends naturally.
    pub fn peek_next_auto(&self) -> Option<String> {
        if self.loop_mode == LoopMode::Single && self.current.is_some() {
            return self.current.clone();
        }
        if let Some(id) = self.up_next.front() {
            return Some(id.clone());
        }
        if let Some(id) = self.order.get(self.next_idx) {
            return Some(id.clone());
        }
        match self.loop_mode {
            LoopMode::All => self.order.first().cloned(),
            LoopMode::Shuffle => self.next_cycle.first().cloned(),
            _ => None,
        }
    }

    /// Move to what `peek_next_auto` returned.
    pub fn advance_auto(&mut self) -> Option<String> {
        if self.loop_mode == LoopMode::Single && self.current.is_some() {
            return self.current.clone();
        }
        self.advance()
    }

    /// Skip forward; ignores `Single`.
    pub fn next_manual(&mut self) -> Option<String> {
        self.advance()
    }

    pub fn prev_manual(&mut self) -> Option<String> {
        // Tracks before the current one; a play-next or removed current sits
        // just before `next_idx`.
        let before = self.cur_idx.unwrap_or(self.next_idx);
        let idx = if before > 0 {
            before - 1
        } else if self.wraps() && !self.order.is_empty() {
            self.order.len() - 1
        } else {
            return None;
        };
        Some(self.go_to(idx))
    }

    pub fn play_next(&mut self, id: &str) {
        self.up_next.push_back(id.to_string());
    }

    /// Remove `id` everywhere. If it was current, `current()` becomes `None`
    /// and the next advance continues from its slot.
    pub fn remove(&mut self, id: &str) -> bool {
        let queued = self.up_next.len();
        self.up_next.retain(|x| x != id);
        let mut found = self.up_next.len() != queued;
        self.base.retain(|x| x != id);
        self.next_cycle.retain(|x| x != id);
        if let Some(i) = self.order.iter().position(|x| x == id) {
            self.order.remove(i);
            found = true;
            if i < self.next_idx {
                self.next_idx -= 1;
            }
            self.cur_idx = match self.cur_idx {
                Some(c) if c == i => None,
                Some(c) if c > i => Some(c - 1),
                other => other,
            };
        }
        if self.current.as_deref() == Some(id) {
            self.current = None;
            self.cur_idx = None;
            found = true;
        }
        found
    }

    /// The context's content changed. Keeps the current track and moves the
    /// cursor to its new position; if it is gone, to the first surviving
    /// upcoming track (else just after the last surviving earlier one).
    pub fn replace_order(&mut self, order: Vec<String>) {
        let (upcoming, earlier, play_order) = {
            let new_ids: HashSet<&str> = order.iter().map(String::as_str).collect();
            let split = self.next_idx.min(self.order.len());
            let upcoming = self.order[split..]
                .iter()
                .find(|id| new_ids.contains(id.as_str()))
                .cloned();
            let earlier = self.order[..split]
                .iter()
                .rev()
                .find(|id| new_ids.contains(id.as_str()))
                .cloned();
            let play_order: Vec<String> = if self.loop_mode == LoopMode::Shuffle {
                // Keep the shuffled sequence; new ids go to the end of the cycle.
                let old_ids: HashSet<&str> = self.order.iter().map(String::as_str).collect();
                let mut added: Vec<String> = order
                    .iter()
                    .filter(|id| !old_ids.contains(id.as_str()))
                    .cloned()
                    .collect();
                added.shuffle(&mut self.rng);
                self.order
                    .iter()
                    .filter(|id| new_ids.contains(id.as_str()))
                    .cloned()
                    .chain(added)
                    .collect()
            } else {
                order.clone()
            };
            (upcoming, earlier, play_order)
        };
        self.base = order;
        self.order = play_order;
        if self.loop_mode == LoopMode::Shuffle {
            self.next_cycle = self.shuffled_base();
        }
        let pos = |id: &str| self.order.iter().position(|x| x == id);
        let cur_idx = self.cur_idx.and(self.current.as_deref()).and_then(pos);
        let next_idx = if let Some(i) = cur_idx {
            i + 1
        } else if let Some(i) = upcoming.as_deref().and_then(pos) {
            i
        } else {
            earlier.as_deref().and_then(pos).map_or(0, |i| i + 1)
        };
        self.cur_idx = cur_idx;
        self.next_idx = next_idx;
    }

    fn wraps(&self) -> bool {
        matches!(self.loop_mode, LoopMode::All | LoopMode::Shuffle)
    }

    /// Forward step shared by auto and manual advance (not `Single` repeat).
    fn advance(&mut self) -> Option<String> {
        if let Some(id) = self.up_next.pop_front() {
            self.current = Some(id.clone());
            self.cur_idx = None;
            return Some(id);
        }
        if self.next_idx >= self.order.len() {
            match self.loop_mode {
                LoopMode::All if !self.order.is_empty() => {}
                LoopMode::Shuffle if !self.next_cycle.is_empty() => {
                    self.order = std::mem::take(&mut self.next_cycle);
                    self.next_cycle = self.shuffled_base();
                }
                _ => return None,
            }
            self.next_idx = 0;
        }
        Some(self.go_to(self.next_idx))
    }

    fn go_to(&mut self, idx: usize) -> String {
        let id = self.order[idx].clone();
        self.current = Some(id.clone());
        self.cur_idx = Some(idx);
        self.next_idx = idx + 1;
        id
    }

    /// Rebuild `order` from `base` for the loop mode (in `Shuffle`: a fresh
    /// permutation with the current track first) and relocate the cursor.
    fn rebuild_order(&mut self) {
        let current = self.current.clone();
        if self.loop_mode == LoopMode::Shuffle {
            let mut order: Vec<String> = self
                .base
                .iter()
                .filter(|id| current.as_ref() != Some(*id))
                .cloned()
                .collect();
            order.shuffle(&mut self.rng);
            if let Some(c) = current.as_ref().filter(|c| self.base.contains(c)) {
                order.insert(0, c.clone());
            }
            self.order = order;
            self.next_cycle = self.shuffled_base();
        } else {
            self.order = self.base.clone();
            self.next_cycle.clear();
        }
        self.cur_idx = current.and_then(|c| self.order.iter().position(|x| *x == c));
        self.next_idx = self.cur_idx.map_or(0, |i| i + 1);
    }

    fn shuffled_base(&mut self) -> Vec<String> {
        let mut order = self.base.clone();
        order.shuffle(&mut self.rng);
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn queue(order: &[&str], start: &str, mode: LoopMode) -> Queue {
        let mut q = Queue::with_seed(7);
        q.set_loop(mode);
        q.start(ContextRef::Album(1), ids(order), start);
        q
    }

    #[test]
    fn starts_at_middle_of_order() {
        let mut q = queue(&["a", "b", "c", "d"], "c", LoopMode::None);
        assert_eq!(q.current(), Some("c"));
        assert_eq!(q.context(), Some(&ContextRef::Album(1)));
        assert_eq!(q.peek_next_auto().as_deref(), Some("d"));
        assert_eq!(q.prev_manual().as_deref(), Some("b"));
        assert_eq!(q.advance_auto().as_deref(), Some("c"));
        assert_eq!(q.advance_auto().as_deref(), Some("d"));
    }

    #[test]
    fn none_mode_stops_after_last_on_auto_and_manual_next() {
        let mut q = queue(&["a", "b"], "b", LoopMode::None);
        assert_eq!(q.peek_next_auto(), None);
        assert_eq!(q.advance_auto(), None);
        assert_eq!(q.next_manual(), None);
        assert_eq!(q.current(), Some("b"));
    }

    #[test]
    fn all_mode_wraps_both_ways() {
        let mut q = queue(&["a", "b", "c"], "c", LoopMode::All);
        assert_eq!(q.peek_next_auto().as_deref(), Some("a"));
        assert_eq!(q.advance_auto().as_deref(), Some("a"));
        assert_eq!(q.prev_manual().as_deref(), Some("c"));
        assert_eq!(q.next_manual().as_deref(), Some("a"));
        assert_eq!(q.current(), Some("a"));
    }

    #[test]
    fn single_auto_repeats_current_but_manual_next_moves_on() {
        let mut q = queue(&["a", "b", "c"], "b", LoopMode::Single);
        assert_eq!(q.loop_mode(), LoopMode::Single);
        assert_eq!(q.peek_next_auto().as_deref(), Some("b"));
        assert_eq!(q.advance_auto().as_deref(), Some("b"));
        assert_eq!(q.current(), Some("b"));
        assert_eq!(q.next_manual().as_deref(), Some("c"));
        assert_eq!(q.next_manual(), None);
    }

    #[test]
    fn play_next_items_come_first_and_are_consumed_once() {
        let mut q = queue(&["a", "b", "c"], "a", LoopMode::None);
        q.play_next("x");
        q.play_next("y");
        assert_eq!(q.peek_next_auto().as_deref(), Some("x"));
        assert_eq!(q.advance_auto().as_deref(), Some("x"));
        assert_eq!(q.next_manual().as_deref(), Some("y"));
        assert_eq!(q.advance_auto().as_deref(), Some("b"));
        assert_eq!(q.advance_auto().as_deref(), Some("c"));
        assert_eq!(q.advance_auto(), None);
    }

    #[test]
    fn prev_manual_at_start_is_none_in_none_mode_and_wraps_in_all() {
        let mut q = queue(&["a", "b", "c"], "a", LoopMode::None);
        assert_eq!(q.prev_manual(), None);
        assert_eq!(q.current(), Some("a"));
        let mut q = queue(&["a", "b", "c"], "a", LoopMode::All);
        assert_eq!(q.prev_manual().as_deref(), Some("c"));
    }

    #[test]
    fn remove_current_then_advance_continues_after_removed_slot() {
        let mut q = queue(&["a", "b", "c", "d"], "b", LoopMode::None);
        assert!(q.remove("b"));
        assert_eq!(q.current(), None);
        assert_eq!(q.peek_next_auto().as_deref(), Some("c"));
        assert_eq!(q.advance_auto().as_deref(), Some("c"));
        assert_eq!(q.prev_manual().as_deref(), Some("a"));
    }

    #[test]
    fn remove_non_current_keeps_current() {
        let mut q = queue(&["a", "b", "c", "d"], "c", LoopMode::None);
        q.play_next("x");
        assert!(q.remove("a"));
        assert!(q.remove("x"));
        assert!(!q.remove("zz"));
        assert_eq!(q.current(), Some("c"));
        assert_eq!(q.advance_auto().as_deref(), Some("d"));
        assert_eq!(q.prev_manual().as_deref(), Some("c"));
        assert_eq!(q.prev_manual().as_deref(), Some("b"));
        assert_eq!(q.prev_manual(), None);
    }

    #[test]
    fn replace_order_keeps_current_by_id() {
        let mut q = queue(&["a", "b", "c", "d"], "b", LoopMode::None);
        q.replace_order(ids(&["d", "c", "b", "a", "e"]));
        assert_eq!(q.current(), Some("b"));
        assert_eq!(q.peek_next_auto().as_deref(), Some("a"));
        assert_eq!(q.advance_auto().as_deref(), Some("a"));
        assert_eq!(q.advance_auto().as_deref(), Some("e"));
        assert_eq!(q.prev_manual().as_deref(), Some("a"));
        assert_eq!(q.prev_manual().as_deref(), Some("b"));
        assert_eq!(q.prev_manual().as_deref(), Some("c"));
    }

    #[test]
    fn shuffle_puts_current_first_visits_each_once_and_reshuffles_on_wrap() {
        let order: Vec<String> = (0..10).map(|i| format!("t{i}")).collect();
        let mut sorted_order = order.clone();
        sorted_order.sort();

        let mut q = Queue::with_seed(42);
        q.set_loop(LoopMode::Shuffle);
        q.start(ContextRef::Library, order.clone(), "t5");
        assert_eq!(q.current(), Some("t5"));
        let mut cycle1 = vec!["t5".to_string()];
        for _ in 1..10 {
            let peeked = q.peek_next_auto();
            let got = q.advance_auto();
            assert_eq!(peeked, got);
            cycle1.push(got.unwrap());
        }
        let mut cycle2 = Vec::new();
        for _ in 0..10 {
            let peeked = q.peek_next_auto();
            let got = q.advance_auto();
            assert_eq!(peeked, got);
            cycle2.push(got.unwrap());
        }
        assert_ne!(cycle1, cycle2);
        for cycle in [&cycle1, &cycle2] {
            let mut sorted = cycle.clone();
            sorted.sort();
            assert_eq!(sorted, sorted_order);
        }

        let mut q = Queue::with_seed(1);
        q.start(ContextRef::Library, order.clone(), "t3");
        q.set_loop(LoopMode::Shuffle);
        assert_eq!(q.current(), Some("t3"));
        let mut seen = vec!["t3".to_string()];
        for _ in 1..10 {
            seen.push(q.advance_auto().unwrap());
        }
        seen.sort();
        assert_eq!(seen, sorted_order);
    }

    #[test]
    fn drop_play_order_behaves_as_normal_order() {
        let mut q = queue(&["new", "a", "b"], "new", LoopMode::None);
        assert_eq!(q.current(), Some("new"));
        assert_eq!(q.prev_manual(), None);
        assert_eq!(q.advance_auto().as_deref(), Some("a"));
        assert_eq!(q.prev_manual().as_deref(), Some("new"));
        assert_eq!(q.next_manual().as_deref(), Some("a"));
        assert_eq!(q.next_manual().as_deref(), Some("b"));
        assert_eq!(q.next_manual(), None);
    }
}
