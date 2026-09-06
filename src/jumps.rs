//! The jump list: `Ctrl-O` back, `Ctrl-I` forward. See `docs/specs/jumplist.md`.
//!
//! Browser-history semantics, not a ring: [`Jumps::push`] while walking
//! (`at < entries.len()`) drops everything strictly newer than the current
//! entry before adding the new one, exactly like following a link from a
//! page you reached with the back button — the tabs that were "forward"
//! from there are gone, because they were never where you were headed. A
//! push that repeats the entry already on top is a no-op, so pressing `n`
//! three times over one search hit records once rather than three
//! unreachable near-duplicates.
//!
//! [`Jumps::back`] pushes `from` first when the list is not already being
//! walked (`at == entries.len()`), i.e. before the very first `Ctrl-O` since
//! the last jump. Without that push there would be nothing to return to:
//! `Ctrl-O` moves `at` off the end of the list, and if the position you were
//! leaving were not recorded there, `Ctrl-I` would have nowhere to bring you
//! back. That push is also what makes `Ctrl-O` `Ctrl-I` a no-op pair, and
//! what `''`/`` ` ` `` reuse to make themselves a toggle.
//!
//! One `Jumps` per window, not per buffer or global, because two windows
//! open on the same buffer are two independent trains of thought — the
//! spec's "The list" section is explicit that `Ctrl-O` in one must not
//! replay the other's history. That is also why entries carry a
//! [`crate::buffer::BufferId`] rather than assuming the window's current
//! buffer: the list follows you across files.
//!
//! Entries are char offsets into a buffer's rope, so an edit upstream of one
//! has to move it or it goes stale; [`Jumps::remap`] applies
//! [`crate::buffer::Edit::map`] to entries in the edited buffer only, the
//! same fold `Editor::settle` already does for selections. [`Jumps::prune`]
//! drops entries whose buffer has since been closed.

use crate::buffer::{BufferId, Edit};

/// A single recorded position: which buffer, and where in it (char offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jump {
    pub buffer: BufferId,
    pub at: usize,
}

/// Oldest-first history of jumps for one window, plus a cursor into it.
///
/// `at == entries.len()` means "not walking" — sitting past the newest
/// entry, at the live position. Any `at < entries.len()` means a `Ctrl-O`
/// walk is in progress; only [`Jumps::push`] resets it back to the end,
/// including the push `back` performs from the live position.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Jumps {
    entries: Vec<Jump>,
    at: usize,
}

/// Vim's default: the oldest entry falls off once a window has jumped this
/// many times.
pub const CAP: usize = 100;

impl Jumps {
    /// Records `jump` as a place to come back to.
    ///
    /// Walking first truncates to the current position (`entries[at]` stays;
    /// only what is strictly newer than it — reachable only by `forward` —
    /// is dropped). This is a browser's history, not a ring: a fresh jump
    /// from the middle discards what used to be "forward" rather than
    /// splicing in beside it. Repeating the top entry is a no-op. The oldest
    /// entry is dropped once the list would grow past [`CAP`]. Either way,
    /// `at` ends back at `entries.len()`: a push always leaves the list not
    /// walking.
    pub fn push(&mut self, jump: Jump) {
        if self.is_walking() {
            self.entries.truncate(self.at + 1);
        }
        if self.entries.last() != Some(&jump) {
            self.entries.push(jump);
            if self.entries.len() > CAP {
                self.entries.remove(0);
            }
        }
        self.at = self.entries.len();
    }

    /// `Ctrl-O`: step back `count` entries, returning where that lands.
    ///
    /// If the list is not already being walked, `from` — the position being
    /// left — is recorded first, so there is something for a later `Ctrl-I`
    /// to return to; the count then steps back from that fresh top entry,
    /// not from beyond it, so a single `Ctrl-O` right after the push lands on
    /// what came before `from`, not on `from` itself. `None` if it could not
    /// move at all (nothing recorded before the current position).
    ///
    /// The recording is done by hand rather than via [`Jumps::push`]: a
    /// count that reaches all the way to the capped end must still see the
    /// oldest entry that was about to fall off, so the target is read out
    /// before the cap trim removes it, not after.
    pub fn back(&mut self, from: Jump, count: usize) -> Option<Jump> {
        if self.is_walking() {
            let before = self.at;
            let at = before.saturating_sub(count);
            if at == before {
                return None;
            }
            self.at = at;
            return Some(self.entries[at]);
        }

        if self.entries.last() != Some(&from) {
            self.entries.push(from);
        }
        let last = self.entries.len() - 1;
        let mut target = last.saturating_sub(count);
        let moved = target != last;
        let result = self.entries[target];
        if self.entries.len() > CAP {
            self.entries.remove(0);
            target = target.saturating_sub(1);
        }
        self.at = target;
        moved.then_some(result)
    }

    /// `Ctrl-I`: step forward `count` entries. `None` if already at the
    /// newest entry — there is nothing more recent to go to.
    pub fn forward(&mut self, count: usize) -> Option<Jump> {
        let last = self.entries.len().checked_sub(1)?;
        if self.at >= last {
            return None;
        }
        self.at = (self.at + count).min(last);
        Some(self.entries[self.at])
    }

    /// `''`/`` ` ` ``: the position before the latest jump, without walking.
    ///
    /// Not walking: the target is the top entry, so push `from` (recording
    /// where you left) and return the old top — the same effect as one
    /// `back`, but expressed as its own toggle: `last` `last` swaps between
    /// two positions forever, rather than continuing to walk further back.
    /// Already walking, this is exactly `back(from, 1)`.
    pub fn last(&mut self, from: Jump) -> Option<Jump> {
        if !self.is_walking() {
            let top = *self.entries.last()?;
            self.push(from);
            return Some(top);
        }
        self.back(from, 1)
    }

    /// Follows entries in `buffer` through edits already applied to it, the
    /// same fold [`Edit::map`] applies to selections — otherwise a jump
    /// recorded above a deleted block would land in the middle of whatever
    /// text slid up to replace it.
    pub fn remap(&mut self, buffer: BufferId, edits: &[Edit]) {
        for entry in &mut self.entries {
            if entry.buffer == buffer {
                entry.at = edits.iter().fold(entry.at, |at, e| e.map(at));
            }
        }
    }

    /// Drops entries whose buffer `open` reports closed, and clamps the walk
    /// cursor back within the shortened list.
    pub fn prune(&mut self, open: impl Fn(BufferId) -> bool) {
        self.entries.retain(|e| open(e.buffer));
        self.at = self.at.min(self.entries.len());
    }

    /// Whether a `Ctrl-O` walk is in progress — `at` short of the live end.
    pub fn is_walking(&self) -> bool {
        self.at < self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{BufferId, Edit, Point};

    fn j(at: usize) -> Jump {
        Jump { buffer: BufferId(0), at }
    }

    #[test]
    fn back_from_the_end_records_where_you_are_so_forward_returns() {
        let mut jumps = Jumps::default();
        jumps.push(j(10)); // left 10 for somewhere
        assert_eq!(jumps.back(j(50), 1), Some(j(10)));
        assert_eq!(jumps.forward(1), Some(j(50)), "Ctrl-O then Ctrl-I is where you started");
        assert_eq!(jumps.forward(1), None, "nothing past the newest");
    }

    #[test]
    fn a_push_while_walking_drops_the_forward_half() {
        let mut jumps = Jumps::default();
        jumps.push(j(1));
        jumps.push(j(2));
        jumps.push(j(3));
        jumps.back(j(4), 2); // at 2, walking
        jumps.push(j(99)); // a fresh jump from the middle
        assert!(!jumps.is_walking());
        assert_eq!(jumps.back(j(100), 1), Some(j(99)));
        assert_eq!(jumps.back(j(100), 1), Some(j(2)), "3 and 4 are gone");
    }

    #[test]
    fn pushing_the_top_again_is_a_no_op() {
        let mut jumps = Jumps::default();
        jumps.push(j(7));
        jumps.push(j(7));
        jumps.push(j(7));
        assert_eq!(jumps.len(), 1);
    }

    #[test]
    fn the_list_is_capped_oldest_first() {
        let mut jumps = Jumps::default();
        for i in 0..(CAP + 5) {
            jumps.push(j(i));
        }
        assert_eq!(jumps.len(), CAP);
        assert_eq!(jumps.back(j(999), CAP), Some(j(5)), "0..5 fell off");
    }

    #[test]
    fn a_count_walks_several_and_saturates() {
        let mut jumps = Jumps::default();
        jumps.push(j(1));
        jumps.push(j(2));
        assert_eq!(jumps.back(j(3), 5), Some(j(1)), "as far as it goes");
        assert_eq!(jumps.back(j(3), 1), None, "and no further");
    }

    #[test]
    fn last_toggles() {
        let mut jumps = Jumps::default();
        jumps.push(j(10));
        assert_eq!(jumps.last(j(50)), Some(j(10)));
        assert_eq!(jumps.last(j(10)), Some(j(50)));
    }

    #[test]
    fn entries_follow_edits_in_their_own_buffer_only() {
        let mut jumps = Jumps::default();
        jumps.push(Jump { buffer: BufferId(0), at: 20 });
        jumps.push(Jump { buffer: BufferId(1), at: 20 });
        // Deleting 5 chars at offset 0 in buffer 0 moves its entry to 15.
        let edit = Edit {
            start_byte: 0,
            old_end_byte: 5,
            new_end_byte: 0,
            start_char: 0,
            old_end_char: 5,
            new_end_char: 0,
            start_point: Point { row: 0, col: 0 },
            old_end_point: Point { row: 0, col: 0 },
            new_end_point: Point { row: 0, col: 0 },
        };
        jumps.remap(BufferId(0), &[edit]);
        assert_eq!(
            jumps.back(Jump { buffer: BufferId(1), at: 0 }, 1),
            Some(Jump { buffer: BufferId(1), at: 20 })
        );
        assert_eq!(
            jumps.back(Jump { buffer: BufferId(1), at: 0 }, 1),
            Some(Jump { buffer: BufferId(0), at: 15 })
        );
    }

    #[test]
    fn closed_buffers_are_pruned() {
        let mut jumps = Jumps::default();
        jumps.push(Jump { buffer: BufferId(0), at: 1 });
        jumps.push(Jump { buffer: BufferId(1), at: 2 });
        jumps.prune(|b| b == BufferId(1));
        assert_eq!(jumps.len(), 1);
        assert_eq!(
            jumps.back(Jump { buffer: BufferId(1), at: 9 }, 1),
            Some(Jump { buffer: BufferId(1), at: 2 })
        );
    }
}
