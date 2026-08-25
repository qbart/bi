//! Where yanked and deleted text goes.
//!
//! Vim gives you 36 addressable slots and makes you choose one at yank time,
//! which is the wrong moment — you rarely know yet whether a thing is worth
//! keeping. This is a deep ring that captures everything automatically; the
//! choice moves to paste time, where a picker can search it.
//!
//! See `docs/specs/registers.md`.

use std::collections::VecDeque;

/// How text was taken, which decides how it goes back in.
///
/// It travels with the text because that is what makes `yy` then `p` open a new
/// line while `yw` then `p` splices inline. The same enum a selection and a
/// region are shaped by — see [`crate::region::Shape`].
pub use crate::region::Shape;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    pub kind: Shape,
}

/// Where an operator's text goes.
///
/// An enum rather than a bool so that named registers and the system clipboard
/// don't rewrite every call site when they land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sink {
    #[default]
    Ring,
    /// `"_` — capture nothing. The escape hatch for throwing away a big junk
    /// block without pushing real history one step closer to the exit.
    BlackHole,
    /// `"+` and `"*` — the system clipboard, through whatever the frontend
    /// supplied. Explicit rather than mirrored onto every yank: a delete is not
    /// a copy, and exporting every `dd` to the desktop is a surprise in the
    /// direction that cannot be undone.
    System,
    /// `"n` — the named space. The name itself is not here: it is asked for
    /// *after* the capture, which is the whole argument of registers.md —
    /// at yank time you do not know yet what a thing is worth calling.
    Named,
}

pub struct Registers {
    /// Front is most recent.
    ring: VecDeque<Entry>,
    bytes: usize,
    capacity: usize,
    byte_budget: usize,
    /// The named space, most recently set first. Separate from the ring and
    /// outside its budget: every entry here was asked for by name, and a
    /// register someone named must not fall off the back of anything.
    named: Vec<(String, Entry)>,
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            ring: VecDeque::new(),
            bytes: 0,
            capacity: 4096,
            byte_budget: 64 << 20,
            named: Vec::new(),
        }
    }
}

impl Registers {
    /// Captures `entry`, evicting from the back to stay within both limits.
    ///
    /// Re-pushing text already held moves it to the front rather than taking a
    /// second slot — repeating a yank should promote it, not duplicate it.
    pub fn push(&mut self, entry: Entry) {
        if let Some(i) = self.ring.iter().position(|e| *e == entry) {
            let old = self.ring.remove(i).expect("index came from the ring");
            self.bytes -= old.text.len();
        }
        self.bytes += entry.text.len();
        self.ring.push_front(entry);

        while self.ring.len() > self.capacity {
            self.evict_oldest();
        }
        // `len() > 1` so an entry too big for the budget is kept rather than
        // dropped — never silently lose what someone copied.
        while self.ring.len() > 1 && self.bytes > self.byte_budget {
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) {
        if let Some(old) = self.ring.pop_back() {
            self.bytes -= old.text.len();
        }
    }

    pub fn front(&self) -> Option<&Entry> {
        self.ring.front()
    }

    #[allow(dead_code, reason = "tests and, later, the picker's count line")]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Most recent first — the order the picker lists them in.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.ring.iter()
    }

    /// By position in that same order, which is what the picker hands back.
    pub fn get(&self, i: usize) -> Option<&Entry> {
        self.ring.get(i)
    }

    /// Stores `entry` under `name`, replacing what held the name before —
    /// a name means one thing, and renaming is re-yanking.
    pub fn set_named(&mut self, name: &str, entry: Entry) {
        self.named.retain(|(held, _)| held != name);
        self.named.insert(0, (name.to_string(), entry));
    }

    /// Most recently named first — the order the picker lists them in.
    pub fn named(&self) -> &[(String, Entry)] {
        &self.named
    }

    /// By position in that same order, which is what the picker hands back.
    pub fn named_at(&self, i: usize) -> Option<&Entry> {
        self.named.get(i).map(|(_, entry)| entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Entry {
        Entry { text: text.into(), kind: Shape::Chars }
    }

    fn lines(text: &str) -> Entry {
        Entry { text: text.into(), kind: Shape::Lines }
    }

    fn small(capacity: usize, byte_budget: usize) -> Registers {
        Registers { capacity, byte_budget, ..Default::default() }
    }

    #[test]
    fn the_most_recent_push_is_the_front() {
        let mut r = Registers::default();
        r.push(chars("first"));
        r.push(chars("second"));
        assert_eq!(r.front(), Some(&chars("second")));
    }

    #[test]
    fn pushing_something_already_held_moves_it_to_the_front() {
        let mut r = Registers::default();
        r.push(chars("a"));
        r.push(chars("b"));
        r.push(chars("a"));

        assert_eq!(r.front(), Some(&chars("a")));
        assert_eq!(r.len(), 2, "not a third slot");
    }

    /// The same text taken two ways pastes two ways, so they are two entries.
    #[test]
    fn text_that_differs_only_in_kind_is_a_separate_entry() {
        let mut r = Registers::default();
        r.push(chars("x"));
        r.push(lines("x"));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn the_oldest_entry_falls_off_the_end_at_capacity() {
        let mut r = small(2, usize::MAX);
        r.push(chars("a"));
        r.push(chars("b"));
        r.push(chars("c"));

        assert_eq!(r.len(), 2);
        assert_eq!(r.front(), Some(&chars("c")));
    }

    #[test]
    fn the_byte_budget_also_evicts_from_the_back() {
        let mut r = small(usize::MAX, 10);
        r.push(chars("aaaaa"));
        r.push(chars("bbbbb"));
        assert_eq!(r.len(), 2, "exactly at budget");

        r.push(chars("ccccc"));
        assert_eq!(r.len(), 2, "the oldest made room");
        assert_eq!(r.front(), Some(&chars("ccccc")));
    }

    #[test]
    fn a_name_holds_one_thing_and_naming_again_replaces_it() {
        let mut r = Registers::default();
        r.set_named("a", chars("first"));
        r.set_named("b", chars("other"));
        r.set_named("a", chars("second"));

        assert_eq!(r.named().len(), 2, "not a third slot");
        assert_eq!(r.named_at(0), Some(&chars("second")), "renamed to the front");
        assert_eq!(r.named_at(1), Some(&chars("other")));
    }

    /// The named space is not the ring: naming evicts nothing and the
    /// budget's evictions never reach a name.
    #[test]
    fn named_entries_live_outside_the_ring_and_its_budget() {
        let mut r = small(1, 10);
        r.set_named("keep", chars("a much longer entry than the budget allows"));
        r.push(chars("bbbbb"));
        r.push(chars("ccccc"));

        assert_eq!(r.len(), 1, "the ring evicted as it always does");
        assert_eq!(r.named_at(0), Some(&chars("a much longer entry than the budget allows")));
    }

    /// Truncating what someone copied is worse than forgetting it, so an entry
    /// that cannot fit is kept anyway — alone.
    #[test]
    fn an_entry_larger_than_the_whole_budget_survives_by_itself() {
        let mut r = small(usize::MAX, 10);
        r.push(chars("aaaaa"));
        r.push(chars("this is very much longer than ten bytes"));

        assert_eq!(r.len(), 1);
        assert_eq!(r.front().unwrap().text.len(), 39);
    }
}
