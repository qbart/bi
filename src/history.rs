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
    /// Where the cursor was before this revision's first change: where undo
    /// puts you back.
    cursor_before: usize,
    /// Where the cursor ended up: where redo puts you.
    cursor_after: usize,
}

pub struct History {
    revisions: Vec<Revision>,
    current: usize,
    /// Changes made since the last commit, not yet a revision.
    pending: Vec<Change>,
    /// Cursor as it was when `pending` started accumulating.
    pending_cursor: usize,
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
                cursor_before: 0,
                cursor_after: 0,
            }],
            current: 0,
            pending: Vec::new(),
            pending_cursor: 0,
            saved: Some(0),
        }
    }
}

impl History {
    /// Adds a change to the group being built. `cursor` is the cursor as it was
    /// *before* the change — only the first one in a group is kept.
    pub fn record(&mut self, change: Change, cursor: usize) {
        if self.pending.is_empty() {
            self.pending_cursor = cursor;
        }
        self.pending.push(change);
    }

    /// Closes the current group into a revision. A no-op when nothing is
    /// pending, so callers can commit at every command boundary without
    /// checking.
    pub fn commit(&mut self, cursor_after: usize) {
        if self.pending.is_empty() {
            return;
        }
        let node = self.revisions.len();
        self.revisions.push(Revision {
            parent: Some(self.current),
            children: Vec::new(),
            changes: std::mem::take(&mut self.pending),
            cursor_before: self.pending_cursor,
            cursor_after,
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
    pub fn undo(&mut self) -> Option<(Vec<Change>, usize)> {
        let rev = &self.revisions[self.current];
        let parent = rev.parent?;
        let changes = rev.changes.iter().rev().map(Change::inverse).collect();
        let cursor = rev.cursor_before;
        self.current = parent;
        Some((changes, cursor))
    }

    /// Changes that walk one step forward along the newest branch, plus the
    /// cursor to restore. `None` at the newest change on this branch.
    pub fn redo(&mut self) -> Option<(Vec<Change>, usize)> {
        let child = *self.revisions[self.current].children.last()?;
        let rev = &self.revisions[child];
        let changes = rev.changes.clone();
        let cursor = rev.cursor_after;
        self.current = child;
        Some((changes, cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(start: usize, text: &str) -> Change {
        Change { start, removed: String::new(), inserted: text.into() }
    }

    /// The property the tree exists for: an edit made after an undo must not
    /// make the undone work unreachable.
    #[test]
    fn editing_after_undo_branches_instead_of_discarding() {
        let mut h = History::default();
        h.record(ins(0, "a"), 0);
        h.commit(1);
        h.record(ins(1, "b"), 1);
        h.commit(2);

        h.undo();
        h.record(ins(1, "c"), 1);
        h.commit(2);

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
        h.record(ins(0, "a"), 0);
        h.commit(1);
        h.undo();
        h.record(ins(0, "z"), 0);
        h.commit(1);
        h.undo();

        let (changes, _) = h.redo().expect("a branch to redo into");
        assert_eq!(changes, vec![ins(0, "z")]);
    }

    #[test]
    fn commit_without_changes_creates_no_revision() {
        let mut h = History::default();
        h.commit(0);
        h.commit(0);
        assert_eq!(h.revisions.len(), 1, "only the root");
    }
}
