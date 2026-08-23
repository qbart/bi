//! Undo history, as a tree.
//!
//! This module owns the shape of the history graph and nothing else — applying
//! a [`Change`] needs the rope, so [`crate::buffer::Buffer`] drives it. The
//! split keeps the graph logic testable without a text buffer.
//!
//! Revisions are only ever appended. Undoing and then editing adds a *second*
//! child to the revision you're sitting on rather than discarding the first, so
//! no keystroke can make earlier work unreachable. Only `u` / `Ctrl-R` are bound
//! today, which walk one branch; the branches the traversal can't yet reach are
//! what `g-` / `g+` would later walk.

/// A reversible replacement, in char indices.
///
/// Char indices, not bytes: this is history's own record, and it inverts by
/// swapping the two strings. [`crate::buffer::Edit`] stays the byte-oriented
/// shape tree-sitter wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
}

impl Change {
    fn inverse(&self) -> Change {
        Change { start: self.start, removed: self.inserted.clone(), inserted: self.removed.clone() }
    }

    /// Char range this change replaces when applied forward.
    pub fn range(&self) -> (usize, usize) {
        (self.start, self.start + self.removed.chars().count())
    }
}

#[derive(Debug)]
struct Revision {
    parent: Option<usize>,
    /// Newest last — `Ctrl-R` follows the most recently created branch.
    children: Vec<usize>,
    /// Applied in order, transforms the parent's text into this revision's.
    changes: Vec<Change>,
    /// Selections as they were before this revision: where undo puts you back.
    before: Cursors,
    /// Selections as they ended up: where redo puts you.
    after: Cursors,
}

/// Selections as plain data — `(anchor, head)` char indices.
///
/// A `Vec` rather than the `Selections` type so that history stays a leaf
/// module: `Selections` knows about `Cursor`, which lives on the far side of
/// `Buffer`, and importing it here would tie the undo tree to the editor's
/// idea of what a cursor is.
pub type Cursors = Vec<(usize, usize)>;

pub struct History {
    revisions: Vec<Revision>,
    current: usize,
    /// Changes made since the last commit, not yet a revision.
    pending: Vec<Change>,
    /// Revision matching what's on disk. `None` once the file has been written
    /// from a state we can no longer identify — currently unreachable, but
    /// `saved` is an `Option` so that a failed write can clear it.
    saved: Option<usize>,
    /// An open group: the `before` cursors of its first commit, and how many
    /// `begin_group`s deep it is. While one is open, `commit` defers — the
    /// changes keep accumulating in `pending` — and closing the group seals
    /// them as one revision. This is how `:g` across four hundred lines is
    /// one `u`: the tree stays append-only, the commit just waits.
    group: Option<(Cursors, usize)>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            revisions: vec![Revision {
                parent: None,
                children: Vec::new(),
                changes: Vec::new(),
                before: Vec::new(),
                after: Vec::new(),
            }],
            current: 0,
            pending: Vec::new(),
            saved: Some(0),
            group: None,
        }
    }
}

impl History {
    /// Adds a change to the group being built.
    ///
    /// Takes no cursor: with several selections the group's first change comes
    /// from whichever one happened to be highest, and that one position is not
    /// the state to restore. The caller passes the whole set at `commit`.
    pub fn record(&mut self, change: Change) {
        self.pending.push(change);
    }

    /// Closes the current group into a revision. A no-op when nothing is
    /// pending, so callers can commit at every command boundary without
    /// checking — and a deferral while a group is open, so a batch command
    /// can run sub-commands that commit blindly and still be one revision.
    pub fn commit(&mut self, before: Cursors, after: Cursors) {
        if self.group.is_some() {
            return;
        }
        self.commit_now(before, after);
    }

    fn commit_now(&mut self, before: Cursors, after: Cursors) {
        if self.pending.is_empty() {
            return;
        }
        let node = self.revisions.len();
        self.revisions.push(Revision {
            parent: Some(self.current),
            children: Vec::new(),
            changes: std::mem::take(&mut self.pending),
            before,
            after,
        });
        self.revisions[self.current].children.push(node);
        self.current = node;
    }

    /// Opens a group, or deepens the one that is open — the outermost caller's
    /// `before` is the one undo restores, because it is where the whole batch
    /// started.
    pub fn begin_group(&mut self, before: Cursors) {
        match &mut self.group {
            Some((_, depth)) => *depth += 1,
            None => self.group = Some((before, 1)),
        }
    }

    /// Closes one level; the outermost close seals the revision.
    pub fn end_group(&mut self, after: Cursors) {
        match self.group.take() {
            Some((before, 1)) => self.commit_now(before, after),
            Some((before, depth)) => self.group = Some((before, depth - 1)),
            None => {}
        }
    }

    /// Force-closes an open group, whatever its depth — the escape hatch
    /// `undo` and `redo` pull, so a `u` replayed mid-batch cannot walk the
    /// tree with half a revision still pending.
    pub fn close_group(&mut self, after: Cursors) {
        if let Some((before, _)) = self.group.take() {
            self.commit_now(before, after);
        }
    }

    /// Records that the current revision is what's on disk.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.current);
    }

    /// Whether the text differs from the last write. Derived, not stored, so
    /// that undoing back to the saved revision correctly reports "no changes".
    pub fn is_modified(&self) -> bool {
        !self.pending.is_empty() || self.saved != Some(self.current)
    }

    /// Changes that walk one step back, already inverted and reversed, plus the
    /// cursor to restore. `None` at the oldest change.
    pub fn undo(&mut self) -> Option<(Vec<Change>, Cursors)> {
        let rev = &self.revisions[self.current];
        let parent = rev.parent?;
        let changes = rev.changes.iter().rev().map(Change::inverse).collect();
        let cursors = rev.before.clone();
        self.current = parent;
        Some((changes, cursors))
    }

    /// Changes that walk one step forward along the newest branch, plus the
    /// cursor to restore. `None` at the newest change on this branch.
    pub fn redo(&mut self) -> Option<(Vec<Change>, Cursors)> {
        let child = *self.revisions[self.current].children.last()?;
        let rev = &self.revisions[child];
        let changes = rev.changes.clone();
        let cursors = rev.after.clone();
        self.current = child;
        Some((changes, cursors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single-cursor set, which is what these cases care about — the whole
    /// set is exercised through the editor.
    fn one(at: usize) -> Cursors {
        vec![(at, at)]
    }

    fn ins(start: usize, text: &str) -> Change {
        Change { start, removed: String::new(), inserted: text.into() }
    }

    /// A group defers the commits inside it into one revision — the batch
    /// commands' whole claim on undo.
    #[test]
    fn a_group_makes_many_commits_one_revision() {
        let mut h = History::default();
        h.begin_group(one(0));
        h.record(ins(0, "a"));
        h.commit(one(1), one(1));
        h.record(ins(1, "b"));
        h.commit(one(2), one(2));
        h.end_group(one(2));

        assert_eq!(h.revisions.len(), 2, "root plus the one the group sealed");
        assert_eq!(h.revisions[1].changes, vec![ins(0, "a"), ins(1, "b")]);
        assert_eq!(h.revisions[1].before, one(0), "undo lands where the batch started");
        assert_eq!(h.revisions[1].after, one(2));
    }

    #[test]
    fn groups_nest_and_the_outermost_seals() {
        let mut h = History::default();
        h.begin_group(one(0));
        h.begin_group(one(9));
        h.record(ins(0, "a"));
        h.end_group(one(9));
        assert_eq!(h.revisions.len(), 1, "the inner close seals nothing");
        h.record(ins(1, "b"));
        h.end_group(one(2));

        assert_eq!(h.revisions.len(), 2);
        assert_eq!(h.revisions[1].before, one(0), "the outer `before` wins");
    }

    #[test]
    fn an_empty_group_leaves_no_revision() {
        let mut h = History::default();
        h.begin_group(one(0));
        h.end_group(one(0));

        assert_eq!(h.revisions.len(), 1);
    }

    /// Undo mid-group must not walk the tree with half a revision pending.
    #[test]
    fn close_group_seals_whatever_depth_was_open() {
        let mut h = History::default();
        h.begin_group(one(0));
        h.begin_group(one(9));
        h.record(ins(0, "a"));
        h.close_group(one(1));

        assert_eq!(h.revisions.len(), 2);
        let (changes, _) = h.undo().expect("the sealed revision undoes");
        assert_eq!(changes.len(), 1);
        h.end_group(one(1));
        assert_eq!(h.revisions.len(), 2, "the stale end_group is harmless");
    }

    /// The property the tree exists for: an edit made after an undo must not
    /// make the undone work unreachable.
    #[test]
    fn editing_after_undo_branches_instead_of_discarding() {
        let mut h = History::default();
        h.record(ins(0, "a"));
        h.commit(one(1), one(1));
        h.record(ins(1, "b"));
        h.commit(one(2), one(2));

        h.undo();
        h.record(ins(1, "c"));
        h.commit(one(2), one(2));

        assert_eq!(h.revisions[1].children, vec![2, 3], "both branches kept");
        assert_eq!(
            h.revisions[2].changes,
            vec![ins(1, "b")],
            "the abandoned branch still holds its changes verbatim"
        );
    }

    #[test]
    fn redo_follows_the_newest_branch() {
        let mut h = History::default();
        h.record(ins(0, "a"));
        h.commit(one(1), one(1));
        h.undo();
        h.record(ins(0, "z"));
        h.commit(one(1), one(1));
        h.undo();

        let (changes, _) = h.redo().expect("a branch to redo into");
        assert_eq!(changes, vec![ins(0, "z")]);
    }

    #[test]
    fn commit_without_changes_creates_no_revision() {
        let mut h = History::default();
        h.commit(one(0), one(0));
        h.commit(one(0), one(0));
        assert_eq!(h.revisions.len(), 1, "only the root");
    }
}
