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
    /// checking.
    pub fn commit(&mut self, before: Cursors, after: Cursors) {
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
