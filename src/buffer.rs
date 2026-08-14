//! Rope-backed text buffer.
//!
//! Two invariants this module exists to protect:
//!
//! 1. The cursor is a **char index** into the rope. Every conversion to bytes
//!    (tree-sitter) or UTF-16 (LSP) happens at the edge, never in motion code.
//! 2. All mutation goes through [`Buffer::apply_edit`], which records an
//!    [`Edit`]. The rope is private specifically so nothing can bypass that.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ropey::Rope;

use crate::history::{Change, History};
use crate::motion::{Kind, Motion, Operator};

/// A position as (row, byte-column-within-row).
///
/// Byte columns, not char columns — this is the shape `tree_sitter::Point`
/// wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub row: usize,
    pub col: usize,
}

/// A single mutation, in the form incremental reparse needs.
///
/// Nothing consumes these yet. They are recorded now because tree-sitter's
/// `InputEdit` and LSP's `textDocument/didChange` both want exactly this at
/// every edit site, and retrofitting it later means touching every mutation.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code, reason = "consumed once tree-sitter and LSP land")]
pub struct Edit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_point: Point,
    pub old_end_point: Point,
    pub new_end_point: Point,
}

/// Where the cursor is, and where it *wants* to be.
///
/// A value, not a field, because more than one of these is the normal case: an
/// operator probes where a motion would land without moving anything, visual
/// mode will want an anchor alongside the head, and split windows each need
/// their own. Position and sticky column travel together because every motion
/// reads and writes both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    /// Char index into the rope.
    pub at: usize,
    /// Sticky column for `j`/`k`, in chars. `None` = recompute from `at`.
    pub goal_col: Option<usize>,
}

impl Cursor {
    /// A cursor at `at` with no sticky column — what every horizontal motion
    /// produces, since moving sideways is what forgets the goal column.
    pub fn at(at: usize) -> Self {
        Self { at, goal_col: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Whitespace,
    Word,
    Punct,
}

fn class_of(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

pub struct Buffer {
    rope: Rope,
    pub path: Option<PathBuf>,
    pub cursor: Cursor,
    /// Drained by tree-sitter / LSP once those exist.
    pub pending_edits: Vec<Edit>,
    history: History,
}

impl Buffer {
    pub fn empty() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            cursor: Cursor::default(),
            pending_edits: Vec::new(),
            history: History::default(),
        }
    }

    /// Opens `path`. A missing file is not an error — it's a new buffer.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut buf = Self::empty();
        if path.exists() {
            let file = File::open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            buf.rope = Rope::from_reader(BufReader::new(file))
                .with_context(|| format!("reading {}", path.display()))?;
        }
        buf.path = Some(path);
        Ok(buf)
    }

    pub fn save(&mut self) -> Result<()> {
        let path = self
            .path
            .clone()
            .context("no file name (use `:w <path>`)")?;
        // What lands on disk has to be a revision, or nothing can be marked as
        // saved and the buffer stays "modified" straight after a good write.
        self.commit_undo();
        let file = File::create(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        self.rope
            .write_to(BufWriter::new(file))
            .with_context(|| format!("writing {}", path.display()))?;
        self.history.mark_saved();
        Ok(())
    }

    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.path = Some(path.as_ref().to_path_buf());
        self.save()
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Whether the text differs from the last write.
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    /// Line count as a human sees it: a file ending in `\n` does not get a
    /// phantom trailing empty line.
    pub fn line_count(&self) -> usize {
        let n = self.rope.len_lines();
        if n > 1 && self.rope.line(n - 1).len_chars() == 0 {
            n - 1
        } else {
            n
        }
    }

    pub fn cursor_row(&self) -> usize {
        self.rope.char_to_line(self.cursor.at)
    }

    pub fn cursor_col(&self) -> usize {
        self.cursor.at - self.rope.line_to_char(self.cursor_row())
    }

    /// Chars in `row`, excluding the line terminator.
    pub fn line_len(&self, row: usize) -> usize {
        if row >= self.rope.len_lines() {
            return 0;
        }
        let line = self.rope.line(row);
        let mut n = line.len_chars();
        if n > 0 && line.char(n - 1) == '\n' {
            n -= 1;
            if n > 0 && line.char(n - 1) == '\r' {
                n -= 1;
            }
        }
        n
    }

    /// Last column the cursor may occupy on `row`.
    ///
    /// Normal mode rests *on* a char; insert mode may sit one past the end.
    fn max_col(&self, row: usize, allow_eol: bool) -> usize {
        let len = self.line_len(row);
        if allow_eol { len } else { len.saturating_sub(1) }
    }

    fn point_at(&self, char_idx: usize) -> Point {
        let char_idx = char_idx.min(self.rope.len_chars());
        let row = self.rope.char_to_line(char_idx);
        let row_start = self.rope.line_to_char(row);
        Point {
            row,
            col: self.rope.char_to_byte(char_idx) - self.rope.char_to_byte(row_start),
        }
    }

    /// Replaces `start..end` (chars) with `text`, logging an [`Edit`] for
    /// incremental reparse. Does **not** touch undo history.
    ///
    /// Undo and redo are the only callers that want this directly — replaying
    /// history must not record itself as new history. Everything else goes
    /// through [`Buffer::apply_edit`].
    fn edit_raw(&mut self, start: usize, end: usize, text: &str) -> Change {
        let start = start.min(self.rope.len_chars());
        let end = end.clamp(start, self.rope.len_chars());

        let start_byte = self.rope.char_to_byte(start);
        let old_end_byte = self.rope.char_to_byte(end);
        let start_point = self.point_at(start);
        let old_end_point = self.point_at(end);

        let removed = if end > start {
            let text = self.rope.slice(start..end).to_string();
            self.rope.remove(start..end);
            text
        } else {
            String::new()
        };
        if !text.is_empty() {
            self.rope.insert(start, text);
        }

        let new_end = start + text.chars().count();
        self.pending_edits.push(Edit {
            start_byte,
            old_end_byte,
            new_end_byte: start_byte + text.len(),
            start_point,
            old_end_point,
            new_end_point: self.point_at(new_end),
        });

        Change { start, removed, inserted: text.to_string() }
    }

    /// The single mutation primitive: replace `start..end` (chars) with `text`
    /// and record it for undo.
    fn apply_edit(&mut self, start: usize, end: usize, text: &str) {
        let change = self.edit_raw(start, end, text);
        self.history.record(change, self.cursor.at);
    }

    // ---- undo --------------------------------------------------------------

    /// Closes the current undo group. A no-op if nothing has changed since the
    /// last one, so callers can commit at every command boundary blindly.
    pub fn commit_undo(&mut self) {
        self.history.commit(self.cursor.at);
    }

    /// Replays `changes` through [`Buffer::edit_raw`], so history traversal
    /// emits [`Edit`]s exactly as ordinary typing does — an undo reaches
    /// tree-sitter and LSP as just another incremental edit.
    fn replay(&mut self, changes: Vec<Change>, cursor: usize) {
        for change in changes {
            let (start, end) = change.range();
            self.edit_raw(start, end, &change.inserted);
        }
        self.cursor = Cursor::at(cursor.min(self.rope.len_chars()));
    }

    /// Steps one revision back. Returns false at the oldest change.
    pub fn undo(&mut self) -> bool {
        self.commit_undo();
        match self.history.undo() {
            Some((changes, cursor)) => {
                self.replay(changes, cursor);
                true
            }
            None => false,
        }
    }

    /// Steps one revision forward, along the most recently created branch.
    /// Returns false at the newest change.
    pub fn redo(&mut self) -> bool {
        self.commit_undo();
        match self.history.redo() {
            Some((changes, cursor)) => {
                self.replay(changes, cursor);
                true
            }
            None => false,
        }
    }

    // ---- editing -----------------------------------------------------------

    pub fn insert_str(&mut self, text: &str) {
        self.apply_edit(self.cursor.at, self.cursor.at, text);
        self.cursor = Cursor::at(self.cursor.at + text.chars().count());
    }

    pub fn insert_char(&mut self, ch: char) {
        let mut b = [0u8; 4];
        self.insert_str(ch.encode_utf8(&mut b));
    }

    /// `x` — delete the char under the cursor, never crossing a line boundary.
    pub fn delete_char_forward(&mut self) {
        let row = self.cursor_row();
        if self.cursor_col() >= self.line_len(row) {
            return;
        }
        self.apply_edit(self.cursor.at, self.cursor.at + 1, "");
        self.cursor.goal_col = None;
        self.clamp(false);
    }

    pub fn backspace(&mut self) {
        if self.cursor.at == 0 {
            return;
        }
        self.apply_edit(self.cursor.at - 1, self.cursor.at, "");
        self.cursor = Cursor::at(self.cursor.at - 1);
    }

    /// `o` / `O`.
    pub fn open_line(&mut self, below: bool) {
        let row = self.cursor_row();
        let at = if below {
            self.rope.line_to_char(row) + self.line_len(row)
        } else {
            self.rope.line_to_char(row)
        };
        self.apply_edit(at, at, "\n");
        self.cursor = Cursor::at(if below { at + 1 } else { at });
    }

    // ---- motions -----------------------------------------------------------
    //
    // These take a cursor and return one; they never touch `self.cursor`. That
    // is what lets an operator ask "where would `w` land?" without moving
    // anything, and it means `w` and `dw` bottom out in the same code and
    // cannot drift apart. The `move_*` wrappers below are the mutating form.

    fn row_of(&self, at: usize) -> usize {
        self.rope.char_to_line(at.min(self.rope.len_chars()))
    }

    fn col_of(&self, at: usize) -> usize {
        at - self.rope.line_to_char(self.row_of(at))
    }

    pub fn clamped(&self, cur: Cursor, allow_eol: bool) -> Cursor {
        let at = cur.at.min(self.rope.len_chars());
        let row = self.row_of(at);
        let max = self.rope.line_to_char(row) + self.max_col(row, allow_eol);
        Cursor { at: at.min(max), goal_col: cur.goal_col }
    }

    pub fn left(&self, cur: Cursor) -> Cursor {
        let start = self.rope.line_to_char(self.row_of(cur.at));
        Cursor::at(if cur.at > start { cur.at - 1 } else { cur.at })
    }

    pub fn right(&self, cur: Cursor, allow_eol: bool) -> Cursor {
        let row = self.row_of(cur.at);
        let max = self.rope.line_to_char(row) + self.max_col(row, allow_eol);
        Cursor::at(if cur.at < max { cur.at + 1 } else { cur.at })
    }

    pub fn vertical(&self, cur: Cursor, delta: isize, allow_eol: bool) -> Cursor {
        let row = self.row_of(cur.at);
        let target = if delta < 0 {
            row.saturating_sub(delta.unsigned_abs())
        } else {
            (row + delta as usize).min(self.line_count().saturating_sub(1))
        };
        if target == row {
            return cur;
        }
        // The goal column outlives the move, which is what makes a run of `j`
        // through a short line come back out at the original column.
        let goal = cur.goal_col.unwrap_or_else(|| self.col_of(cur.at));
        Cursor {
            at: self.rope.line_to_char(target) + goal.min(self.max_col(target, allow_eol)),
            goal_col: Some(goal),
        }
    }

    pub fn line_start(&self, cur: Cursor) -> Cursor {
        Cursor::at(self.rope.line_to_char(self.row_of(cur.at)))
    }

    pub fn line_end(&self, cur: Cursor, allow_eol: bool) -> Cursor {
        let row = self.row_of(cur.at);
        Cursor::at(self.rope.line_to_char(row) + self.max_col(row, allow_eol))
    }

    pub fn at_row(&self, row: usize, allow_eol: bool) -> Cursor {
        let row = row.min(self.line_count().saturating_sub(1));
        self.clamped(Cursor::at(self.rope.line_to_char(row)), allow_eol)
    }

    /// `w` — start of the next word.
    pub fn word_forward(&self, cur: Cursor, allow_eol: bool) -> Cursor {
        let len = self.rope.len_chars();
        let mut i = cur.at;
        if i >= len {
            return cur;
        }
        let start_class = class_of(self.rope.char(i));
        if start_class != CharClass::Whitespace {
            while i < len && class_of(self.rope.char(i)) == start_class {
                i += 1;
            }
        }
        while i < len && class_of(self.rope.char(i)) == CharClass::Whitespace {
            i += 1;
        }
        self.clamped(Cursor::at(i.min(len)), allow_eol)
    }

    /// `b` — start of the previous word.
    pub fn word_backward(&self, cur: Cursor, allow_eol: bool) -> Cursor {
        if cur.at == 0 {
            return cur;
        }
        let mut i = cur.at - 1;
        while i > 0 && class_of(self.rope.char(i)) == CharClass::Whitespace {
            i -= 1;
        }
        let class = class_of(self.rope.char(i));
        if class != CharClass::Whitespace {
            while i > 0 && class_of(self.rope.char(i - 1)) == class {
                i -= 1;
            }
        }
        self.clamped(Cursor::at(i), allow_eol)
    }

    // ---- operators ---------------------------------------------------------

    /// Where `motion` lands, applied `count` times from `from`.
    ///
    /// `$` resolves against the last char of the line; every other motion is
    /// allowed one past it, because `w` on the final word of a buffer has to be
    /// able to name the position after it or `dw` would leave a char behind.
    fn motion_target(&self, motion: Motion, count: usize, from: Cursor) -> Cursor {
        let eol = motion != Motion::LineEnd;
        match motion {
            Motion::FirstLine => return self.at_row(0, eol),
            Motion::LastLine => return self.at_row(usize::MAX, eol),
            Motion::Line(n) => return self.at_row(n.saturating_sub(1), eol),
            _ => {}
        }
        let mut cur = from;
        for _ in 0..count {
            cur = match motion {
                Motion::Left => self.left(cur),
                Motion::Right => self.right(cur, eol),
                Motion::Up => self.vertical(cur, -1, eol),
                Motion::Down => self.vertical(cur, 1, eol),
                Motion::WordForward => self.word_forward(cur, eol),
                Motion::WordBackward => self.word_backward(cur, eol),
                Motion::LineStart => self.line_start(cur),
                Motion::LineEnd => self.line_end(cur, eol),
                Motion::CurrentLine | Motion::FirstLine | Motion::LastLine | Motion::Line(_) => cur,
            };
        }
        cur
    }

    /// End of the `count`-th word from `at`, or `None` if `at` is whitespace.
    ///
    /// This is `e`, and it exists only to serve the `cw` quirk below.
    fn word_end_from(&self, at: usize, count: usize) -> Option<usize> {
        let len = self.rope.len_chars();
        if at >= len || class_of(self.rope.char(at)) == CharClass::Whitespace {
            return None;
        }
        let mut i = at;
        for _ in 0..count {
            while i < len && class_of(self.rope.char(i)) == CharClass::Whitespace {
                i += 1;
            }
            if i >= len {
                break;
            }
            let class = class_of(self.rope.char(i));
            while i + 1 < len && class_of(self.rope.char(i + 1)) == class {
                i += 1;
            }
            i += 1;
        }
        Some(i.saturating_sub(1))
    }

    /// The char range `op` + `motion` covers, and whether it was linewise.
    ///
    /// `None` when the motion goes nowhere — `b` at the start of the buffer,
    /// say — so the caller can leave the text alone rather than record an empty
    /// edit in the undo history.
    fn operator_range(&self, op: Operator, motion: Motion, count: usize) -> Option<(usize, usize)> {
        let count = count.max(1);
        let len = self.rope.len_chars();

        if motion.kind() == Kind::Linewise {
            let start_row = self.cursor_row();
            let last_row = self.line_count().saturating_sub(1);
            let target_row = match motion {
                Motion::CurrentLine => start_row + count - 1,
                _ => self.row_of(self.motion_target(motion, count, self.cursor).at),
            };
            let (first, last) = (start_row.min(target_row), start_row.max(target_row).min(last_row));

            let content_start = self.rope.line_to_char(first);
            // `cc` empties the lines but leaves them, so insert mode has a line
            // to sit on; `dd` takes the terminator too.
            if op == Operator::Change {
                return Some((content_start, self.rope.line_to_char(last) + self.line_len(last)));
            }
            let end = if last + 1 < self.rope.len_lines() {
                self.rope.line_to_char(last + 1)
            } else {
                len
            };
            // Deleting through the final line takes the *preceding* newline, or
            // the file keeps a stray empty line.
            return Some(if end == len && content_start > 0 {
                (content_start - 1, end)
            } else {
                (content_start, end)
            });
        }

        // Vim quirk: `cw` on a non-blank is `ce` — it changes the word without
        // swallowing the whitespace after it. On whitespace it is plain `w`.
        if op == Operator::Change
            && motion == Motion::WordForward
            && let Some(end) = self.word_end_from(self.cursor.at, count)
        {
            return Some((self.cursor.at, (end + 1).min(len)));
        }

        let target = self.motion_target(motion, count, self.cursor).at;
        let (lo, mut hi) = (self.cursor.at.min(target), self.cursor.at.max(target));
        if motion.kind() == Kind::Inclusive {
            hi = (hi + 1).min(len);
        }
        // Vim quirk: an exclusive motion that leaves the line stops at the end
        // of it, so `dw` on the last word of a line does not join the next one.
        if motion == Motion::WordForward && self.row_of(hi) > self.row_of(lo) {
            let row = self.row_of(lo);
            hi = self.rope.line_to_char(row) + self.line_len(row);
        }
        (hi > lo).then_some((lo, hi))
    }

    /// Applies `op` over `motion`. Returns whether anything changed.
    pub fn operate(&mut self, op: Operator, motion: Motion, count: usize) -> bool {
        let Some((start, end)) = self.operator_range(op, motion, count) else {
            return false;
        };
        self.apply_edit(start, end, "");
        self.cursor = Cursor::at(start);
        // Delete lands back on a char; change leaves the cursor where the text
        // was, which for `d$` or `cc` is one past the end of what's left.
        self.clamp(op == Operator::Change);
        true
    }

    // ---- motions, mutating form --------------------------------------------

    pub fn clamp(&mut self, allow_eol: bool) {
        self.cursor = self.clamped(self.cursor, allow_eol);
    }

    /// Moves the cursor by `motion`. The mutating counterpart of the pure
    /// motions above — `Action::Move` is the only caller.
    pub fn apply_motion(&mut self, motion: Motion, allow_eol: bool) {
        self.cursor = match motion {
            Motion::Left => self.left(self.cursor),
            Motion::Right => self.right(self.cursor, allow_eol),
            Motion::Up => self.vertical(self.cursor, -1, allow_eol),
            Motion::Down => self.vertical(self.cursor, 1, allow_eol),
            Motion::WordForward => self.word_forward(self.cursor, allow_eol),
            Motion::WordBackward => self.word_backward(self.cursor, allow_eol),
            Motion::LineStart => self.line_start(self.cursor),
            Motion::LineEnd => self.line_end(self.cursor, allow_eol),
            Motion::FirstLine => self.at_row(0, allow_eol),
            Motion::LastLine => self.at_row(usize::MAX, allow_eol),
            Motion::Line(n) => self.at_row(n.saturating_sub(1), allow_eol),
            Motion::CurrentLine => self.cursor,
        };
    }

    pub fn goto_row(&mut self, row: usize, allow_eol: bool) {
        self.cursor = self.at_row(row, allow_eol);
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loads `text` the way `open` does — straight into the rope, so the
    /// fixture leaves no undo history or pending edits behind.
    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str(text);
        b
    }

    #[test]
    fn trailing_newline_is_not_a_line() {
        assert_eq!(buf("a\nb\n").line_count(), 2);
        assert_eq!(buf("a\nb").line_count(), 2);
        assert_eq!(buf("").line_count(), 1);
    }

    #[test]
    fn normal_mode_cursor_stays_on_a_char() {
        let mut b = buf("abc\ndef");
        b.apply_motion(Motion::LineEnd, false);
        assert_eq!(b.cursor_col(), 2);
        b.apply_motion(Motion::LineEnd, true);
        assert_eq!(b.cursor_col(), 3);
    }

    #[test]
    fn vertical_motion_keeps_a_goal_column() {
        let mut b = buf("longer line\nab\nlonger line");
        b.goto_row(0, false);
        b.apply_motion(Motion::LineEnd, false);
        let goal = b.cursor_col();
        b.apply_motion(Motion::Down, false);
        assert_eq!(b.cursor_col(), 1, "clamped to the short line");
        b.apply_motion(Motion::Down, false);
        assert_eq!(b.cursor_col(), goal, "restored on the long line");
    }

    #[test]
    fn word_motions_cross_punctuation_and_whitespace() {
        let mut b = buf("foo.bar  baz");
        b.apply_motion(Motion::WordForward, false);
        assert_eq!(b.cursor.at, 3, "stops at the dot");
        b.apply_motion(Motion::WordForward, false);
        assert_eq!(b.cursor.at, 4, "then the word after it");
        b.apply_motion(Motion::WordForward, false);
        assert_eq!(b.cursor.at, 9, "skips the double space");
        b.apply_motion(Motion::WordBackward, false);
        assert_eq!(b.cursor.at, 4);
    }

    #[test]
    fn x_does_not_eat_the_newline() {
        let mut b = buf("ab\ncd");
        b.cursor = Cursor::at(1);
        b.delete_char_forward();
        assert_eq!(b.rope().to_string(), "a\ncd");
        // Deleting the last char of a line drags the cursor back onto a char.
        assert_eq!(b.cursor_col(), 0);

        // On an empty line there is nothing under the cursor to take.
        let mut b = buf("\ncd");
        b.delete_char_forward();
        assert_eq!(b.rope().to_string(), "\ncd");
    }

    #[test]
    fn dd_on_the_last_line_leaves_no_empty_line() {
        let mut b = buf("a\nb\nc");
        b.goto_row(2, false);
        b.operate(Operator::Delete, Motion::CurrentLine, 1);
        assert_eq!(b.rope().to_string(), "a\nb");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.cursor_row(), 1);
    }

    #[test]
    fn dd_on_the_only_line_empties_the_buffer() {
        let mut b = buf("only");
        b.operate(Operator::Delete, Motion::CurrentLine, 1);
        assert_eq!(b.rope().to_string(), "");
        assert_eq!(b.cursor.at, 0);
    }

    #[test]
    fn open_line_below_lands_on_the_new_line() {
        let mut b = buf("a\nc");
        b.open_line(true);
        assert_eq!(b.rope().to_string(), "a\n\nc");
        assert_eq!(b.cursor_row(), 1);
    }

    #[test]
    fn undo_restores_text_and_cursor() {
        let mut b = buf("hello");
        b.cursor = Cursor::at(5);
        b.insert_str(" world");
        b.commit_undo();
        assert_eq!(b.rope().to_string(), "hello world");

        assert!(b.undo());
        assert_eq!(b.rope().to_string(), "hello");
        assert_eq!(b.cursor.at, 5, "back where the change started");
    }

    #[test]
    fn redo_reapplies_what_undo_took_back() {
        let mut b = buf("hello");
        b.cursor = Cursor::at(5);
        b.insert_str(" world");
        b.commit_undo();
        b.undo();

        assert!(b.redo());
        assert_eq!(b.rope().to_string(), "hello world");
        assert_eq!(b.cursor.at, 11, "where the change left the cursor");
    }

    #[test]
    fn undo_and_redo_stop_at_the_ends() {
        let mut b = buf("x");
        assert!(!b.undo(), "nothing to undo on a fresh buffer");

        b.cursor = Cursor::at(1);
        b.insert_str("y");
        b.commit_undo();
        assert!(!b.redo(), "nothing to redo until something is undone");

        assert!(b.undo());
        assert!(!b.undo(), "already at the oldest change");
        assert!(b.redo());
        assert!(!b.redo(), "already at the newest change");
    }

    /// Undoing an *insertion* is the direction that catches char/byte mixups:
    /// the replayed change has to remove exactly the chars that went in, and
    /// "ñé" is 2 chars but 4 bytes.
    #[test]
    fn undo_of_a_multibyte_insertion_removes_exactly_those_chars() {
        let mut b = buf("ab\ncd");
        b.cursor = Cursor::at(1);
        b.insert_str("ñé");
        b.commit_undo();
        assert_eq!(b.rope().to_string(), "añéb\ncd");

        b.undo();
        assert_eq!(b.rope().to_string(), "ab\ncd");
        assert_eq!(b.cursor.at, 1);
    }

    #[test]
    fn undo_of_a_multibyte_deletion_puts_the_text_back() {
        let mut b = buf("añb\ncd");
        b.cursor = Cursor::at(1);
        b.delete_char_forward();
        b.commit_undo();
        assert_eq!(b.rope().to_string(), "ab\ncd");

        b.undo();
        assert_eq!(b.rope().to_string(), "añb\ncd");
        assert_eq!(b.cursor.at, 1);
    }

    /// Undo has to reach tree-sitter and LSP as an ordinary incremental edit,
    /// not as a signal to reparse the world.
    #[test]
    fn undo_logs_an_edit_like_any_other_mutation() {
        let mut b = buf("é\n");
        b.cursor = Cursor::at(1);
        b.insert_str("x");
        b.commit_undo();
        b.pending_edits.clear();

        b.undo();

        let e = b.pending_edits.last().expect("undo logged no edit");
        // Taking "x" back out: char 1 is byte 2, and the "x" spanned byte 2..3.
        assert_eq!(e.start_byte, 2);
        assert_eq!(e.old_end_byte, 3);
        assert_eq!(e.new_end_byte, 2);
        assert_eq!(e.start_point, Point { row: 0, col: 2 });
        assert_eq!(e.old_end_point, Point { row: 0, col: 3 });
        assert_eq!(e.new_end_point, Point { row: 0, col: 2 });
    }

    /// The reason `modified` is derived from history rather than stored: undoing
    /// your way back to what's on disk means there is nothing to save.
    #[test]
    fn undoing_back_to_the_saved_state_clears_modified() {
        let mut b = buf("hello");
        assert!(!b.is_modified(), "freshly loaded");

        b.cursor = Cursor::at(5);
        b.insert_str("!");
        b.commit_undo();
        assert!(b.is_modified());

        b.undo();
        assert!(!b.is_modified(), "back at the state on disk");

        b.redo();
        assert!(b.is_modified(), "and away from it again");
    }

    #[test]
    fn an_uncommitted_edit_still_counts_as_modified() {
        let mut b = buf("hello");
        b.cursor = Cursor::at(5);
        b.insert_str("!");
        assert!(b.is_modified(), "mid-insert, before the group closes");
    }

    /// A write has to close the open group, otherwise the text on disk matches
    /// no revision and the buffer keeps claiming it has unsaved changes.
    #[test]
    fn saving_with_an_open_group_marks_the_written_state_as_saved() {
        let path = std::env::temp_dir().join("bee_save_open_group.txt");
        let _ = std::fs::remove_file(&path);

        let mut b = buf("hello");
        b.cursor = Cursor::at(5);
        b.insert_str("!");
        b.save_as(&path).expect("write failed");

        assert!(!b.is_modified(), "the buffer is what's on disk");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello!");
        let _ = std::fs::remove_file(&path);
    }

    // ---- operators ---------------------------------------------------------

    fn op(text: &str, at: usize, op: Operator, m: Motion, count: usize) -> String {
        let mut b = buf(text);
        b.cursor = Cursor::at(at);
        b.operate(op, m, count);
        b.rope().to_string()
    }

    #[test]
    fn dw_deletes_to_the_start_of_the_next_word() {
        assert_eq!(op("foo bar baz", 0, Operator::Delete, Motion::WordForward, 1), "bar baz");
    }

    #[test]
    fn dw_on_the_last_word_takes_all_of_it() {
        assert_eq!(op("foo", 0, Operator::Delete, Motion::WordForward, 1), "");
    }

    #[test]
    fn counted_dw_deletes_that_many_words() {
        assert_eq!(op("a b c d", 0, Operator::Delete, Motion::WordForward, 3), "d");
    }

    /// Vim quirk: `cw` on a non-blank behaves like `ce`, so the whitespace after
    /// the word survives. A literal `w` would have eaten it.
    #[test]
    fn cw_changes_the_word_and_leaves_the_spaces() {
        assert_eq!(op("foo   bar", 0, Operator::Change, Motion::WordForward, 1), "   bar");
    }

    #[test]
    fn cw_on_whitespace_is_not_special_and_acts_like_w() {
        assert_eq!(op("a   bar", 1, Operator::Change, Motion::WordForward, 1), "abar");
    }

    #[test]
    fn counted_cw_reaches_the_end_of_the_last_word() {
        assert_eq!(op("foo   bar baz", 0, Operator::Change, Motion::WordForward, 2), " baz");
    }

    /// Vim quirk: `dw` at the end of a line stops there instead of pulling the
    /// next line up.
    #[test]
    fn dw_at_the_end_of_a_line_does_not_join_lines() {
        assert_eq!(op("hello\nworld", 3, Operator::Delete, Motion::WordForward, 1), "hel\nworld");
    }

    #[test]
    fn d_dollar_deletes_through_the_last_char_but_not_the_newline() {
        assert_eq!(op("hello world\nnext", 6, Operator::Delete, Motion::LineEnd, 1), "hello \nnext");
    }

    #[test]
    fn d_zero_deletes_back_to_the_line_start() {
        assert_eq!(op("hello world", 6, Operator::Delete, Motion::LineStart, 1), "world");
    }

    #[test]
    fn db_deletes_the_previous_word() {
        assert_eq!(op("foo bar", 4, Operator::Delete, Motion::WordBackward, 1), "bar");
    }

    #[test]
    fn dl_takes_the_char_under_the_cursor() {
        assert_eq!(op("abc", 1, Operator::Delete, Motion::Right, 1), "ac");
    }

    #[test]
    fn dj_is_linewise_and_takes_both_lines() {
        assert_eq!(op("one\ntwo\nthree", 1, Operator::Delete, Motion::Down, 1), "three");
    }

    #[test]
    fn dk_is_linewise_upward() {
        assert_eq!(op("one\ntwo\nthree", 5, Operator::Delete, Motion::Up, 1), "three");
    }

    #[test]
    fn dgg_deletes_from_the_first_line_through_this_one() {
        assert_eq!(op("one\ntwo\nthree", 5, Operator::Delete, Motion::FirstLine, 1), "three");
    }

    #[test]
    fn dd_takes_the_whole_line_including_its_newline() {
        assert_eq!(op("one\ntwo\nthree", 1, Operator::Delete, Motion::CurrentLine, 1), "two\nthree");
    }

    #[test]
    fn counted_dd_takes_that_many_lines() {
        assert_eq!(op("one\ntwo\nthree", 1, Operator::Delete, Motion::CurrentLine, 2), "three");
    }

    #[test]
    fn dd_past_the_end_stops_at_the_last_line() {
        assert_eq!(op("one\ntwo", 0, Operator::Delete, Motion::CurrentLine, 99), "");
    }

    /// `cc` empties the line but keeps it, so insert mode has somewhere to go.
    #[test]
    fn cc_clears_the_line_without_removing_it() {
        assert_eq!(op("one\ntwo", 1, Operator::Change, Motion::CurrentLine, 1), "\ntwo");
    }

    #[test]
    fn an_operator_that_moves_nowhere_changes_nothing() {
        let mut b = buf("abc");
        b.cursor = Cursor::at(0);
        assert!(!b.operate(Operator::Delete, Motion::WordBackward, 1), "b at char 0 has no range");
        assert_eq!(b.rope().to_string(), "abc");
    }

    #[test]
    fn delete_leaves_the_cursor_on_a_char() {
        let mut b = buf("foo bar");
        b.cursor = Cursor::at(4);
        b.operate(Operator::Delete, Motion::LineEnd, 1);
        assert_eq!(b.rope().to_string(), "foo ");
        assert_eq!(b.cursor.at, 3, "pulled back onto the last remaining char");
    }

    #[test]
    fn change_leaves_the_cursor_where_the_text_was() {
        let mut b = buf("foo bar");
        b.cursor = Cursor::at(4);
        b.operate(Operator::Change, Motion::LineEnd, 1);
        assert_eq!(b.rope().to_string(), "foo ");
        assert_eq!(b.cursor.at, 4, "sitting past the end, ready to type");
    }

    #[test]
    fn edits_record_byte_ranges_for_incremental_reparse() {
        // Multi-byte on purpose: char index 1 is byte 2.
        let mut b = buf("é\n");
        b.cursor = Cursor::at(1);
        b.insert_str("x");

        let e = b.pending_edits.last().unwrap();
        assert_eq!(e.start_byte, 2);
        assert_eq!(e.old_end_byte, 2);
        assert_eq!(e.new_end_byte, 3);
        assert_eq!(e.start_point, Point { row: 0, col: 2 });
        assert_eq!(e.new_end_point, Point { row: 0, col: 3 });
    }

    #[test]
    fn deleting_across_a_newline_reports_both_points() {
        let mut b = buf("ab\ncd");
        b.cursor = Cursor::at(3);
        b.backspace();

        let e = b.pending_edits.last().unwrap();
        assert_eq!(e.start_point, Point { row: 0, col: 2 });
        assert_eq!(e.old_end_point, Point { row: 1, col: 0 });
        assert_eq!(e.new_end_point, Point { row: 0, col: 2 });
        assert_eq!(b.rope().to_string(), "abcd");
    }
}
