//! Selections — the thing the editor works in, rather than a position.
//!
//! Normal mode is every selection collapsed, visual mode is one selection with
//! room between its ends, and multi-cursor is more than one. Making this the
//! primitive turns two features into one piece of machinery; see
//! `docs/specs/selections.md`.

use crate::buffer::Cursor;

/// A range of the buffer with a direction.
///
/// Collapsed when the ends meet, which is what normal mode is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection started. Stays put while the head moves.
    pub anchor: Cursor,
    /// Where the cursor is. Motions move this end.
    pub head: Cursor,
}

impl Selection {
    /// A collapsed selection — a plain cursor.
    pub fn at(at: usize) -> Self {
        let cursor = Cursor::at(at);
        Self { anchor: cursor, head: cursor }
    }

    pub fn collapsed(cursor: Cursor) -> Self {
        Self { anchor: cursor, head: cursor }
    }

    pub fn is_collapsed(self) -> bool {
        self.anchor.at == self.head.at
    }

    /// Low..high regardless of which way round the ends are, which is what
    /// every operator wants. Direction only matters to `o` and to motions.
    pub fn range(self) -> (usize, usize) {
        (self.anchor.at.min(self.head.at), self.anchor.at.max(self.head.at))
    }

    /// The range an operator covers, with the character under the head
    /// included — charwise visual is inclusive in vim.
    pub fn inclusive_range(self, len: usize) -> (usize, usize) {
        let (lo, hi) = self.range();
        (lo, (hi + 1).min(len))
    }

    /// Swaps the ends, so the other one can be adjusted. Vim's `o`.
    pub fn flipped(self) -> Self {
        Self { anchor: self.head, head: self.anchor }
    }

    fn overlaps(self, other: Self) -> bool {
        let (a_lo, a_hi) = self.range();
        let (b_lo, b_hi) = other.range();
        // Touching counts: two collapsed selections on the same character are
        // the same cursor, and both would otherwise type twice.
        a_lo <= b_hi && b_lo <= a_hi
    }

    fn merged(self, other: Self) -> Self {
        let (lo, _) = self.range();
        let (_, hi) = other.range();
        // Keeps the first one's direction, which is the one the user started.
        if self.head.at >= self.anchor.at {
            Self { anchor: Cursor::at(lo), head: Cursor::at(hi) }
        } else {
            Self { anchor: Cursor::at(hi), head: Cursor::at(lo) }
        }
    }
}

/// The editor's selections. Never empty, sorted, never overlapping.
///
/// The invariants are held here rather than at each call site, because every
/// one of them is a bug that only shows up with two cursors and is miserable to
/// track down from the symptom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selections {
    list: Vec<Selection>,
    primary: usize,
}

impl Default for Selections {
    fn default() -> Self {
        Self { list: vec![Selection::at(0)], primary: 0 }
    }
}

impl Selections {
    pub fn single(cursor: Cursor) -> Self {
        Self { list: vec![Selection::collapsed(cursor)], primary: 0 }
    }

    /// The selection the viewport follows and the status line reports.
    pub fn primary(&self) -> Selection {
        self.list[self.primary]
    }

    pub fn primary_mut(&mut self) -> &mut Selection {
        &mut self.list[self.primary]
    }

    /// Where the one visible terminal cursor goes.
    pub fn cursor(&self) -> Cursor {
        self.primary().head
    }

    pub fn all(&self) -> &[Selection] {
        &self.list
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// Replaces the set, restoring the invariants.
    ///
    /// An empty `list` keeps what was there: there is always a cursor, and
    /// "remove the last selection" is not something a caller may express.
    pub fn set(&mut self, list: Vec<Selection>) {
        if list.is_empty() {
            return;
        }
        // Captured before the swap: the old index means nothing against the new
        // list, but the old *selection* still says which one to follow.
        let was = self.primary();
        self.list = list;
        self.normalise(was);
    }

    pub fn push(&mut self, selection: Selection) {
        // The new one becomes primary, so the viewport follows what was just
        // added rather than staying where it was.
        self.list.push(selection);
        self.primary = self.list.len() - 1;
        self.normalise(selection);
    }

    /// Drops everything but the primary. `Esc` in multi-cursor.
    pub fn collapse_to_primary(&mut self) {
        let keep = self.primary();
        self.list = vec![keep];
        self.primary = 0;
    }

    /// Collapses every selection onto its head, leaving the cursors in place.
    /// Leaving visual mode.
    pub fn collapse_each(&mut self) {
        for selection in &mut self.list {
            selection.anchor = selection.head;
        }
    }

    /// Sorts and merges what now overlaps.
    ///
    /// `follow` is the selection `primary` should end up pointing at, given by
    /// identity rather than by index — sorting and merging both move indices,
    /// so an index captured beforehand means nothing afterwards.
    fn normalise(&mut self, follow: Selection) {
        self.list.sort_by_key(|s| s.range());

        let mut merged: Vec<Selection> = Vec::with_capacity(self.list.len());
        for selection in self.list.drain(..) {
            match merged.last_mut() {
                Some(last) if last.overlaps(selection) => *last = last.merged(selection),
                _ => merged.push(selection),
            }
        }
        self.list = merged;

        // The primary may have been merged into a neighbour; the selection that
        // swallowed it is the right answer.
        let (flo, fhi) = follow.range();
        self.primary = self
            .list
            .iter()
            .position(|s| {
                let (lo, hi) = s.range();
                lo <= flo && fhi <= hi
            })
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sels(positions: &[usize]) -> Selections {
        let mut s = Selections::default();
        s.set(positions.iter().map(|&p| Selection::at(p)).collect());
        s
    }

    #[test]
    fn a_fresh_set_is_one_collapsed_cursor_at_the_start() {
        let s = Selections::default();
        assert_eq!(s.len(), 1);
        assert!(s.primary().is_collapsed());
        assert_eq!(s.cursor().at, 0);
    }

    #[test]
    fn the_set_can_never_be_emptied() {
        let mut s = sels(&[5]);
        s.set(vec![]);
        assert_eq!(s.len(), 1, "there is always a cursor");
        assert_eq!(s.cursor().at, 5);
    }

    #[test]
    fn selections_are_kept_sorted() {
        let s = sels(&[30, 10, 20]);
        let heads: Vec<usize> = s.all().iter().map(|s| s.head.at).collect();
        assert_eq!(heads, vec![10, 20, 30]);
    }

    #[test]
    fn two_cursors_on_the_same_spot_become_one() {
        let s = sels(&[7, 7]);
        assert_eq!(s.len(), 1, "or typing would insert twice");
    }

    #[test]
    fn overlapping_ranges_merge_into_their_union() {
        let mut s = Selections::default();
        s.set(vec![
            Selection { anchor: Cursor::at(0), head: Cursor::at(10) },
            Selection { anchor: Cursor::at(5), head: Cursor::at(20) },
        ]);
        assert_eq!(s.len(), 1);
        assert_eq!(s.primary().range(), (0, 20));
    }

    #[test]
    fn selections_that_only_touch_at_the_ends_still_merge() {
        let mut s = Selections::default();
        s.set(vec![
            Selection { anchor: Cursor::at(0), head: Cursor::at(5) },
            Selection { anchor: Cursor::at(5), head: Cursor::at(9) },
        ]);
        assert_eq!(s.len(), 1, "a shared boundary is still an overlap");
    }

    #[test]
    fn disjoint_selections_are_left_alone() {
        let s = sels(&[0, 10, 20]);
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn a_range_reads_low_to_high_whichever_way_it_was_made() {
        let forward = Selection { anchor: Cursor::at(3), head: Cursor::at(9) };
        let backward = Selection { anchor: Cursor::at(9), head: Cursor::at(3) };
        assert_eq!(forward.range(), (3, 9));
        assert_eq!(backward.range(), (3, 9));
    }

    #[test]
    fn flipping_swaps_the_ends_without_moving_the_range() {
        let s = Selection { anchor: Cursor::at(3), head: Cursor::at(9) };
        let f = s.flipped();
        assert_eq!(f.head.at, 3);
        assert_eq!(f.anchor.at, 9);
        assert_eq!(f.range(), s.range());
    }

    #[test]
    fn pushing_makes_the_new_selection_primary() {
        let mut s = sels(&[10]);
        s.push(Selection::at(50));
        assert_eq!(s.cursor().at, 50, "the viewport follows what was just added");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn collapsing_to_primary_keeps_only_that_one() {
        let mut s = sels(&[10, 20, 30]);
        s.push(Selection::at(40));
        s.collapse_to_primary();
        assert_eq!(s.len(), 1);
        assert_eq!(s.cursor().at, 40);
    }

    #[test]
    fn collapsing_each_drops_the_anchors_and_keeps_the_heads() {
        let mut s = Selections::default();
        s.set(vec![
            Selection { anchor: Cursor::at(0), head: Cursor::at(4) },
            Selection { anchor: Cursor::at(10), head: Cursor::at(14) },
        ]);
        s.collapse_each();
        assert!(s.all().iter().all(|s| s.is_collapsed()));
        let heads: Vec<usize> = s.all().iter().map(|s| s.head.at).collect();
        assert_eq!(heads, vec![4, 14]);
    }

    #[test]
    fn selections_that_land_on_the_same_place_merge() {
        let mut s = sels(&[10, 12]);
        // Both moved to the same spot, as a shared motion target would do.
        s.set(vec![Selection::at(5), Selection::at(5)]);
        assert_eq!(s.len(), 1);
        assert_eq!(s.cursor().at, 5);
    }

    #[test]
    fn primary_survives_being_merged_into_a_neighbour() {
        let mut s = sels(&[10, 20]);
        s.push(Selection::at(30));
        assert_eq!(s.cursor().at, 30);
        // Swallow the primary inside a bigger selection.
        s.set(vec![Selection::at(10), Selection { anchor: Cursor::at(25), head: Cursor::at(35) }]);
        assert_eq!(s.primary().range(), (25, 35), "the selection that ate it is primary now");
    }
}
