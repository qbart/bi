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
    /// Char index into the rope.
    pub cursor: usize,
    /// Sticky column for `j`/`k`, in chars. `None` = recompute from cursor.
    pub goal_col: Option<usize>,
    pub modified: bool,
    /// Drained by tree-sitter / LSP once those exist.
    pub pending_edits: Vec<Edit>,
}

impl Buffer {
    pub fn empty() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            cursor: 0,
            goal_col: None,
            modified: false,
            pending_edits: Vec::new(),
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
        let file = File::create(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        self.rope
            .write_to(BufWriter::new(file))
            .with_context(|| format!("writing {}", path.display()))?;
        self.modified = false;
        Ok(())
    }

    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.path = Some(path.as_ref().to_path_buf());
        self.save()
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
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
        self.rope.char_to_line(self.cursor)
    }

    pub fn cursor_col(&self) -> usize {
        self.cursor - self.rope.line_to_char(self.cursor_row())
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

    /// The single mutation primitive: replace `start..end` (chars) with `text`.
    fn apply_edit(&mut self, start: usize, end: usize, text: &str) {
        let start = start.min(self.rope.len_chars());
        let end = end.clamp(start, self.rope.len_chars());

        let start_byte = self.rope.char_to_byte(start);
        let old_end_byte = self.rope.char_to_byte(end);
        let start_point = self.point_at(start);
        let old_end_point = self.point_at(end);

        if end > start {
            self.rope.remove(start..end);
        }
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
        self.modified = true;
    }

    // ---- editing -----------------------------------------------------------

    pub fn insert_str(&mut self, text: &str) {
        self.apply_edit(self.cursor, self.cursor, text);
        self.cursor += text.chars().count();
        self.goal_col = None;
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
        self.apply_edit(self.cursor, self.cursor + 1, "");
        self.clamp(false);
        self.goal_col = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.apply_edit(self.cursor - 1, self.cursor, "");
        self.cursor -= 1;
        self.goal_col = None;
    }

    /// `dd` — delete the cursor's line, terminator included.
    pub fn delete_line(&mut self) {
        let row = self.cursor_row();
        let start = self.rope.line_to_char(row);
        let end = if row + 1 < self.rope.len_lines() {
            self.rope.line_to_char(row + 1)
        } else {
            self.rope.len_chars()
        };

        // Deleting the last line takes the *preceding* newline instead, so the
        // file doesn't keep a stray empty line.
        let (start, end) = if end == self.rope.len_chars() && start > 0 {
            (start - 1, end)
        } else {
            (start, end)
        };

        self.apply_edit(start, end, "");
        self.cursor = self.rope.line_to_char(row.min(self.line_count().saturating_sub(1)));
        self.clamp(false);
        self.goal_col = None;
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
        self.cursor = if below { at + 1 } else { at };
        self.goal_col = None;
    }

    // ---- motions -----------------------------------------------------------

    pub fn clamp(&mut self, allow_eol: bool) {
        self.cursor = self.cursor.min(self.rope.len_chars());
        let row = self.cursor_row();
        let start = self.rope.line_to_char(row);
        let max = start + self.max_col(row, allow_eol);
        if self.cursor > max {
            self.cursor = max;
        }
    }

    pub fn move_left(&mut self) {
        let row = self.cursor_row();
        let start = self.rope.line_to_char(row);
        if self.cursor > start {
            self.cursor -= 1;
        }
        self.goal_col = None;
    }

    pub fn move_right(&mut self, allow_eol: bool) {
        let row = self.cursor_row();
        let max = self.rope.line_to_char(row) + self.max_col(row, allow_eol);
        if self.cursor < max {
            self.cursor += 1;
        }
        self.goal_col = None;
    }

    pub fn move_vertical(&mut self, delta: isize, allow_eol: bool) {
        let row = self.cursor_row();
        let target = if delta < 0 {
            row.saturating_sub(delta.unsigned_abs())
        } else {
            (row + delta as usize).min(self.line_count().saturating_sub(1))
        };
        if target == row {
            return;
        }
        let goal = self.goal_col.unwrap_or_else(|| self.cursor_col());
        self.cursor = self.rope.line_to_char(target) + goal.min(self.max_col(target, allow_eol));
        self.goal_col = Some(goal);
    }

    pub fn move_line_start(&mut self) {
        self.cursor = self.rope.line_to_char(self.cursor_row());
        self.goal_col = None;
    }

    pub fn move_line_end(&mut self, allow_eol: bool) {
        let row = self.cursor_row();
        self.cursor = self.rope.line_to_char(row) + self.max_col(row, allow_eol);
        self.goal_col = None;
    }

    pub fn goto_row(&mut self, row: usize, allow_eol: bool) {
        let row = row.min(self.line_count().saturating_sub(1));
        self.cursor = self.rope.line_to_char(row);
        self.goal_col = None;
        self.clamp(allow_eol);
    }

    /// `w` — start of the next word.
    pub fn move_word_forward(&mut self, allow_eol: bool) {
        let len = self.rope.len_chars();
        let mut i = self.cursor;
        if i >= len {
            return;
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
        self.cursor = i.min(len);
        self.goal_col = None;
        self.clamp(allow_eol);
    }

    /// `b` — start of the previous word.
    pub fn move_word_backward(&mut self, allow_eol: bool) {
        if self.cursor == 0 {
            return;
        }
        let mut i = self.cursor - 1;
        while i > 0 && class_of(self.rope.char(i)) == CharClass::Whitespace {
            i -= 1;
        }
        let class = class_of(self.rope.char(i));
        if class != CharClass::Whitespace {
            while i > 0 && class_of(self.rope.char(i - 1)) == class {
                i -= 1;
            }
        }
        self.cursor = i;
        self.goal_col = None;
        self.clamp(allow_eol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        b.insert_str(text);
        b.cursor = 0;
        b.pending_edits.clear();
        b.modified = false;
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
        b.move_line_end(false);
        assert_eq!(b.cursor_col(), 2);
        b.move_line_end(true);
        assert_eq!(b.cursor_col(), 3);
    }

    #[test]
    fn vertical_motion_keeps_a_goal_column() {
        let mut b = buf("longer line\nab\nlonger line");
        b.goto_row(0, false);
        b.move_line_end(false);
        let goal = b.cursor_col();
        b.move_vertical(1, false);
        assert_eq!(b.cursor_col(), 1, "clamped to the short line");
        b.move_vertical(1, false);
        assert_eq!(b.cursor_col(), goal, "restored on the long line");
    }

    #[test]
    fn word_motions_cross_punctuation_and_whitespace() {
        let mut b = buf("foo.bar  baz");
        b.move_word_forward(false);
        assert_eq!(b.cursor, 3, "stops at the dot");
        b.move_word_forward(false);
        assert_eq!(b.cursor, 4, "then the word after it");
        b.move_word_forward(false);
        assert_eq!(b.cursor, 9, "skips the double space");
        b.move_word_backward(false);
        assert_eq!(b.cursor, 4);
    }

    #[test]
    fn x_does_not_eat_the_newline() {
        let mut b = buf("ab\ncd");
        b.cursor = 1;
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
        b.delete_line();
        assert_eq!(b.rope().to_string(), "a\nb");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.cursor_row(), 1);
    }

    #[test]
    fn dd_on_the_only_line_empties_the_buffer() {
        let mut b = buf("only");
        b.delete_line();
        assert_eq!(b.rope().to_string(), "");
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn open_line_below_lands_on_the_new_line() {
        let mut b = buf("a\nc");
        b.open_line(true);
        assert_eq!(b.rope().to_string(), "a\n\nc");
        assert_eq!(b.cursor_row(), 1);
    }

    #[test]
    fn edits_record_byte_ranges_for_incremental_reparse() {
        // Multi-byte on purpose: char index 1 is byte 2.
        let mut b = buf("é\n");
        b.cursor = 1;
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
        b.cursor = 3;
        b.backspace();

        let e = b.pending_edits.last().unwrap();
        assert_eq!(e.start_point, Point { row: 0, col: 2 });
        assert_eq!(e.old_end_point, Point { row: 1, col: 0 });
        assert_eq!(e.new_end_point, Point { row: 0, col: 2 });
        assert_eq!(b.rope().to_string(), "abcd");
    }
}
