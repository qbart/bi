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
/// line while `yw` then `p` splices inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A span within a line. Pastes inline.
    Charwise,
    /// Whole lines. Always stored with a trailing newline, even when it came
    /// from a final line that had none.
    Linewise,
    /// A rectangle. Rows are joined with `\n` and there is no trailing one —
    /// the newlines separate the rows of the block rather than terminating
    /// lines of the buffer, which is what makes it go back in as a rectangle.
    Blockwise,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    pub kind: EntryKind,
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
}

pub struct Registers {
    /// Front is most recent.
    ring: VecDeque<Entry>,
    bytes: usize,
    capacity: usize,
    byte_budget: usize,
}

impl Default for Registers {
    fn default() -> Self {
        Self { ring: VecDeque::new(), bytes: 0, capacity: 4096, byte_budget: 64 << 20 }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Entry {
        Entry { text: text.into(), kind: EntryKind::Charwise }
    }

    fn lines(text: &str) -> Entry {
        Entry { text: text.into(), kind: EntryKind::Linewise }
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
