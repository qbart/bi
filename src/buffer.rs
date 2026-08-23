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

use crate::history::{Change, Cursors, History};
use crate::indent::{self, Indent};
use crate::motion::{Kind, Motion, Operator, Target, TextObject};
use crate::registers::{Entry, Shape};
use crate::trim::Trim;

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
/// Bytes and points are what tree-sitter's `InputEdit` and LSP's
/// `textDocument/didChange` want. The char range beside them is for positions:
/// cursors are char indices, so a window that is not the one editing follows
/// the text through these rather than being clamped to its new length.
///
/// One record with both, because there is one drain — see `Editor::settle`.
#[derive(Debug, Clone, Copy)]

pub struct Edit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_char: usize,
    pub old_end_char: usize,
    pub new_end_char: usize,
    pub start_point: Point,
    pub old_end_point: Point,
    pub new_end_point: Point,
}

impl Edit {
    /// Where `at` ends up after this edit.
    ///
    /// Positions before the edit do not move; positions after it shift by the
    /// length delta; a position *inside* what was replaced collapses to the
    /// start, since the text it named is gone.
    pub fn map(&self, at: usize) -> usize {
        if at <= self.start_char {
            at
        } else if at >= self.old_end_char {
            at - self.old_end_char + self.new_end_char
        } else {
            self.start_char
        }
    }
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

/// A buffer's identity within a session, stable across deletions.
///
/// Handed out monotonically and never reused. `:bd` in the middle of the list
/// must not silently repoint every window one file to the left, which is
/// exactly what an index into a `Vec` would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(pub u32);

pub struct Buffer {
    rope: Rope,
    pub path: Option<PathBuf>,
    /// Drained by `Editor::settle` — for tree-sitter and LSP `didChange` both.
    pub pending_edits: Vec<Edit>,
    history: History,
    /// Bumped by every mutation, undo and redo included. A cache over the text
    /// — the search count is the first — compares it to know it is stale.
    /// Not a version anyone may reason about beyond "different means changed".
    edits: u64,
}

impl Buffer {
    pub fn empty() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            pending_edits: Vec::new(),
            history: History::default(),
            edits: 0,
        }
    }

    /// Opens `path`. A missing file is not an error — it's a new buffer.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut buf = Self::empty();
        if path.exists() {
            let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
            buf.rope = Rope::from_reader(BufReader::new(file))
                .with_context(|| format!("reading {}", path.display()))?;
        }
        buf.path = Some(path);
        Ok(buf)
    }

    /// Takes the selections because a write closes the open undo group, and a
    /// group needs somewhere to put you when you undo back through it.
    pub fn save(&mut self, before: Cursors, after: Cursors) -> Result<()> {
        let path = self.path.clone().context("no file name (use `:w <path>`)")?;
        // What lands on disk has to be a revision, or nothing can be marked as
        // saved and the buffer stays "modified" straight after a good write.
        self.commit_undo(before, after);
        let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
        self.rope
            .write_to(BufWriter::new(file))
            .with_context(|| format!("writing {}", path.display()))?;
        self.history.mark_saved();
        Ok(())
    }

    pub fn save_as(
        &mut self,
        before: Cursors,
        after: Cursors,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        self.path = Some(path.as_ref().to_path_buf());
        self.save(before, after)
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
        if n > 1 && self.rope.line(n - 1).len_chars() == 0 { n - 1 } else { n }
    }

    /// Row of `at`. Public because everything above the buffer works in
    /// selections now and has to ask.
    pub fn row_at(&self, at: Cursor) -> usize {
        self.rope.char_to_line(at.at.min(self.rope.len_chars()))
    }

    pub fn col_at(&self, at: Cursor) -> usize {
        at.at - self.rope.line_to_char(self.row_at(at))
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
        Point { row, col: self.rope.char_to_byte(char_idx) - self.rope.char_to_byte(row_start) }
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

        self.edits += 1;
        let new_end = start + text.chars().count();
        self.pending_edits.push(Edit {
            start_byte,
            old_end_byte,
            new_end_byte: start_byte + text.len(),
            start_char: start,
            old_end_char: end,
            new_end_char: new_end,
            start_point,
            old_end_point,
            new_end_point: self.point_at(new_end),
        });

        Change { start, removed, inserted: text.to_string() }
    }

    /// The single mutation primitive: replace `start..end` (chars) with `text`
    /// and record it for undo.
    ///
    /// No cursor: with several selections the group's first change comes from
    /// whichever one happened to be highest, and that position is not the state
    /// undo should restore. `commit_undo` takes the whole set instead.
    fn apply_edit(&mut self, start: usize, end: usize, text: &str) {
        let change = self.edit_raw(start, end, text);
        self.history.record(change);
    }

    // ---- undo --------------------------------------------------------------

    /// Closes the current undo group. A no-op if nothing has changed since the
    /// last one, so callers can commit at every command boundary blindly.
    pub fn commit_undo(&mut self, before: Cursors, after: Cursors) {
        self.history.commit(before, after);
    }

    /// Opens an undo group: every `commit_undo` until `end_undo_group` is
    /// deferred into one revision. `:g` and `:normal` run sub-commands that
    /// commit at their own boundaries, and this is what makes the batch one
    /// `u` anyway. Nests; the outermost pair wins.
    pub fn begin_undo_group(&mut self, before: Cursors) {
        self.history.begin_group(before);
    }

    pub fn end_undo_group(&mut self, after: Cursors) {
        self.history.end_group(after);
    }

    /// Replays `changes` through [`Buffer::edit_raw`], so history traversal
    /// emits [`Edit`]s exactly as ordinary typing does — an undo reaches
    /// tree-sitter and LSP as just another incremental edit.
    fn replay(&mut self, changes: Vec<Change>, cursors: Cursors) -> Cursors {
        for change in changes {
            let (start, end) = change.range();
            self.edit_raw(start, end, &change.inserted);
        }
        let len = self.rope.len_chars();
        cursors.into_iter().map(|(a, h)| (a.min(len), h.min(len))).collect()
    }

    /// Steps one revision back. Returns false at the oldest change.
    /// `None` at the oldest change. `Some` carries the selections to restore.
    pub fn undo(&mut self, before: Cursors, after: Cursors) -> Option<Cursors> {
        // A `u` replayed mid-batch seals the group where it stands: undo
        // must not walk the tree with half a revision still pending.
        self.history.close_group(after.clone());
        self.commit_undo(before, after);
        let (changes, cursors) = self.history.undo()?;
        Some(self.replay(changes, cursors))
    }

    /// Steps one revision forward, along the most recently created branch.
    /// Returns false at the newest change.
    pub fn redo(&mut self, before: Cursors, after: Cursors) -> Option<Cursors> {
        self.history.close_group(after.clone());
        self.commit_undo(before, after);
        let (changes, cursors) = self.history.redo()?;
        Some(self.replay(changes, cursors))
    }

    // ---- editing -----------------------------------------------------------

    pub fn insert_str(&mut self, at: Cursor, text: &str) -> Cursor {
        self.apply_edit(at.at, at.at, text);
        Cursor::at(at.at + text.chars().count())
    }

    pub fn insert_char(&mut self, at: Cursor, ch: char) -> Cursor {
        let mut b = [0u8; 4];
        self.insert_str(at, ch.encode_utf8(&mut b))
    }

    pub fn backspace(&mut self, at: Cursor) -> Cursor {
        if at.at == 0 {
            return at;
        }
        self.apply_edit(at.at - 1, at.at, "");
        Cursor::at(at.at - 1)
    }

    /// `r{char}` — overwrites `count` chars in place.
    ///
    /// Returns false and changes nothing when the line has fewer than `count`
    /// characters left, which is vim's behaviour: `3rx` on a two-character tail
    /// is a no-op, not a partial replace.
    pub fn replace_chars(&mut self, at: Cursor, ch: char, count: usize) -> Option<Cursor> {
        let row = self.row_at(at);
        if self.line_len(row) < self.col_at(at) + count {
            return None;
        }
        let text: String = std::iter::repeat_n(ch, count).collect();
        self.apply_edit(at.at, at.at + count, &text);
        // Vim leaves the cursor on the last character it replaced.
        Some(Cursor::at(at.at + text.chars().count().saturating_sub(1)))
    }

    /// `~` — flips the case under the cursor and steps right, `count` times.
    ///
    /// Stops at the end of the line rather than wrapping onto the next.
    /// Characters with no case are stepped over, not skipped: `~` on a digit
    /// still advances, as vim does.
    pub fn toggle_case(&mut self, at: Cursor, count: usize) -> Cursor {
        let mut cursor = at;
        for _ in 0..count {
            let row = self.row_at(cursor);
            if self.col_at(cursor) >= self.line_len(row) {
                break;
            }
            let ch = self.rope.char(cursor.at);
            // `ß` uppercases to `SS`, so the replacement is not always one char.
            let flipped: String = if ch.is_lowercase() {
                ch.to_uppercase().collect()
            } else if ch.is_uppercase() {
                ch.to_lowercase().collect()
            } else {
                cursor = Cursor::at(cursor.at + 1);
                continue;
            };
            let width = flipped.chars().count();
            self.apply_edit(cursor.at, cursor.at + 1, &flipped);
            cursor = Cursor::at(cursor.at + width);
        }
        self.clamped(cursor, false)
    }

    /// `J` — joins the line below onto this one, `count` lines' worth.
    ///
    /// The newline and the next line's indent become a single space. No space
    /// is added when this line already ends in whitespace or the next line is
    /// blank. `1J` and `2J` both join one line, as in vim.
    pub fn join_lines(&mut self, at: Cursor, count: usize) -> Cursor {
        let mut cursor = at;
        for _ in 0..count.max(2) - 1 {
            let row = self.row_at(cursor);
            if row + 1 >= self.line_count() {
                break;
            }
            let line_start = self.rope.line_to_char(row);
            let end = line_start + self.line_len(row);
            let next_start = self.rope.line_to_char(row + 1);
            let next_len = self.line_len(row + 1);

            let mut indent = 0;
            while indent < next_len && matches!(self.rope.char(next_start + indent), ' ' | '\t') {
                indent += 1;
            }

            let ends_blank = end == line_start || matches!(self.rope.char(end - 1), ' ' | '\t');
            let sep = if ends_blank || next_len == indent { "" } else { " " };

            self.apply_edit(end, next_start + indent, sep);
            // Vim leaves the cursor on the join.
            cursor = Cursor::at(end);
        }
        self.clamped(cursor, false)
    }

    /// `:e` — re-reads the file from disk, discarding everything local.
    ///
    /// Undo history goes with it: the old revisions describe text that no
    /// longer exists, and replaying them through `edit_raw` would desynchronise
    /// the parse tree. Vim keeps history here behind `'undoreload'`; bi does
    /// not. The caller must rebuild syntax — the tree belongs to the old text.
    pub fn reload(&mut self, at: Cursor) -> Result<Cursor> {
        let path = self.path.clone().context("no file name")?;
        // Keep the cursor where the new text still has room for it. Clamping
        // the raw char index is not enough: it can land on the phantom row
        // after a trailing newline, which `line_count` does not count.
        let (row, col) = (self.row_at(at), self.col_at(at));
        *self = Self::open(&path)?;
        let row = row.min(self.line_count().saturating_sub(1));
        let col = col.min(self.max_col(row, false));
        Ok(Cursor::at(self.rope.line_to_char(row) + col))
    }

    /// Replaces a char range with `text`. What `:case` needs and what nothing
    /// else did: every other rewrite here is an operator with a rule of its
    /// own about where the cursor lands.
    pub fn replace_range(&mut self, start: usize, end: usize, text: &str) {
        self.apply_edit(start, end, text);
    }

    // ---- surroundings ------------------------------------------------------

    /// The char range a target covers, without touching it.
    ///
    /// Asked as a yank, because a yank is the operator that changes nothing:
    /// `cw`'s quirk of stopping at the end of a word belongs to `c` and would
    /// be wrong for `ysw`.
    pub fn range_of(&self, at: Cursor, target: Target, count: usize) -> Option<(usize, usize)> {
        let (mut start, mut end) = self.operator_range(at, Operator::Yank, target, count)?;
        // A linewise target carries a line terminator — the one after it, or
        // the one before it on the last line of a file that does not end in
        // one. That is right for a yank and wrong for a surround, which would
        // put the closing string on the next line.
        let newline = |i: usize| matches!(self.rope.char(i), '\n' | '\r');
        while end > start && newline(end - 1) {
            end -= 1;
        }
        while start < end && newline(start) {
            start += 1;
        }
        (end > start).then_some((start, end))
    }

    /// The two delimiter positions of the innermost `ch` pair the cursor is
    /// inside, as an outer range.
    ///
    /// Built from the *inner* object rather than the outer one, because `a"`
    /// reaches for the whitespace beside the quotes — right for `da"` and
    /// wrong for `ds"`, which must take the quotes and nothing else. Every
    /// pair bi surrounds with is one character on each side, so the delimiters
    /// are exactly the characters either side of the innards.
    pub fn surround_of(&self, at: Cursor, ch: char) -> Option<(usize, usize)> {
        let object = crate::surround::object_for(ch)?;
        let (start, end) = self.object_range(at, object, false)?;
        (start >= 1 && end < self.rope.len_chars() + 1).then_some((start - 1, end + 1))
    }

    /// Wraps `start..end` in `pair`, and says where the cursor goes.
    ///
    /// The closing string first, so that inserting the opening one cannot
    /// invalidate the position the closing one was going to. The cursor rides
    /// on whatever was inserted to its left and nowhere else — which is the
    /// whole point of the command.
    pub fn surround(
        &mut self,
        at: Cursor,
        start: usize,
        end: usize,
        pair: &crate::surround::Pair,
    ) -> Cursor {
        let base = self.pending_edits.len();
        self.apply_edit(end, end, &pair.close);
        self.apply_edit(start, start, &pair.open);
        self.moved_across(at, base)
    }

    /// `ds{ch}` — removes the innermost `ch` pair around the cursor.
    pub fn unsurround(&mut self, at: Cursor, ch: char) -> Option<Cursor> {
        let (open, close) = self.surround_of(at, ch)?;
        let base = self.pending_edits.len();
        self.apply_edit(close - 1, close, "");
        self.apply_edit(open, open + 1, "");
        Some(self.moved_across(at, base))
    }

    /// `cs{from}{to}` — replaces one pair with another, in place.
    pub fn resurround(
        &mut self,
        at: Cursor,
        from: char,
        pair: &crate::surround::Pair,
    ) -> Option<Cursor> {
        let (open, close) = self.surround_of(at, from)?;
        let base = self.pending_edits.len();
        self.apply_edit(close - 1, close, &pair.close);
        self.apply_edit(open, open + 1, &pair.open);
        Some(self.moved_across(at, base))
    }

    /// Where `at` ends up after the edits made since `base`.
    fn moved_across(&self, at: Cursor, base: usize) -> Cursor {
        let moved = self.pending_edits[base..].iter().fold(at.at, |at, edit| edit.map(at));
        self.clamped(Cursor::at(moved), false)
    }

    /// The text of `row`, without its terminator.
    fn line_text(&self, row: usize) -> String {
        if row >= self.rope.len_lines() {
            return String::new();
        }
        let start = self.rope.line_to_char(row);
        self.rope.slice(start..start + self.line_len(row)).to_string()
    }

    /// The leading whitespace of `row`, as it is written — tabs stay tabs.
    ///
    /// Autoindent copies characters rather than a width, which is what keeps a
    /// tab-indented file tab-indented under `expandtab`: the new line matches
    /// its neighbour, and only `>` or `Tab` change the indent character.
    fn copied_indent(&self, row: usize, indent: &Indent) -> String {
        if !indent.autoindent {
            return String::new();
        }
        indent::leading(&self.line_text(row)).to_string()
    }

    /// `o` / `O`, with the indent of the line it opened from.
    pub fn open_line(&mut self, at: Cursor, below: bool, indent: &Indent) -> Cursor {
        let row = self.row_at(at);
        let lead = self.copied_indent(row, indent);
        let start = if below {
            self.rope.line_to_char(row) + self.line_len(row)
        } else {
            self.rope.line_to_char(row)
        };
        // Above, the newline goes *after* the indent: the text being pushed
        // down is the old line, and the indent belongs to the new one.
        let text = if below { format!("\n{lead}") } else { format!("{lead}\n") };
        self.apply_edit(start, start, &text);
        Cursor::at(start + if below { 1 + lead.chars().count() } else { lead.chars().count() })
    }

    /// `Enter` in insert mode: splits the line and indents what comes after it
    /// to match what came before.
    pub fn insert_newline(&mut self, at: Cursor, indent: &Indent) -> Cursor {
        let lead = self.copied_indent(self.row_at(at), indent);
        self.insert_str(at, &format!("\n{lead}"))
    }

    // ---- indentation -------------------------------------------------------
    //
    // Widths are columns, never characters: a line indented with a tab and a
    // space is 5 columns wide at `tab_width = 4`, and `<` takes it to 0 rather
    // than to "one character less". See `docs/specs/indent.md`.

    /// The rows `target` touches, for the operators that are always linewise.
    ///
    /// `None` when the target names nothing — a `f` that misses, an object the
    /// cursor is not inside — so `>f;` on a line without a `;` leaves the file
    /// alone rather than indenting the line the cursor happens to be on.
    pub fn target_rows(&self, at: Cursor, target: Target, count: usize) -> Option<(usize, usize)> {
        let count = count.max(1);
        match target {
            Target::Motion(Motion::FindChar { ch, forward, till, repeat }) => {
                let to = self.find_char(at, ch, forward, till, repeat, count)?;
                let (a, b) = (self.row_of(at.at), self.row_of(to.at));
                Some((a.min(b), a.max(b)))
            }
            Target::Motion(motion) => Some(self.linewise_rows(at, motion, count)),
            Target::Object { object, around } => {
                let (start, end) = self.object_range(at, object, around)?;
                Some((self.row_of(start), self.row_of(end.saturating_sub(1).max(start))))
            }
        }
    }

    /// Moves rows `first..=last` by `steps` indent steps, and says where the
    /// cursor goes: the first non-blank of the first row, as vim does, so a
    /// repeated `>>` keeps pushing the line it is looking at.
    ///
    /// `None` when nothing moved — a `<` on a file already at column zero puts
    /// no no-op on the undo stack.
    ///
    /// A line with nothing but whitespace on it is not indented: pushing an
    /// empty line right buys nothing and fills the diff with `    `. `<` still
    /// empties one, which is the way to get rid of it.
    pub fn indent_rows(
        &mut self,
        first: usize,
        last: usize,
        right: bool,
        steps: usize,
        indent: &Indent,
    ) -> Option<Cursor> {
        let by = indent.step() * steps.max(1);
        let last = last.min(self.line_count().saturating_sub(1));
        let mut changed = false;

        // Bottom-up, so an edit on one row cannot move the rows still to come.
        for row in (first..=last).rev() {
            let text = self.line_text(row);
            let lead = indent::leading(&text);
            if right && lead.len() == text.len() {
                continue;
            }
            let width = indent::width_of(lead, indent.tab_width);
            let width = if right { width + by } else { width.saturating_sub(by) };
            let new = indent.render(width);
            if new == lead {
                continue;
            }
            let start = self.rope.line_to_char(row);
            self.apply_edit(start, start + lead.chars().count(), &new);
            changed = true;
        }

        changed.then(|| self.first_non_blank(self.clamped(self.at_row(first, false), false)))
    }

    /// Rewraps rows `first..=last` to `width` columns — `gq`, through
    /// [`crate::reflow::reflow`]. Says where the cursor goes: the first
    /// non-blank of the last row produced, the end of what was formatted.
    /// `None` when nothing changed, so no no-op lands on the undo stack.
    pub fn reflow_rows(
        &mut self,
        first: usize,
        last: usize,
        width: usize,
        tab_width: usize,
    ) -> Option<Cursor> {
        let last = last.min(self.line_count().saturating_sub(1));
        let lines: Vec<String> = (first..=last).map(|row| self.line(row)).collect();
        let wrapped = crate::reflow::reflow(&lines, width, tab_width);
        if wrapped == lines {
            return None;
        }
        let start = self.rope.line_to_char(first);
        let stop = self.rope.line_to_char(last) + self.line_len(last);
        self.apply_edit(start, stop, &wrapped.join("\n"));
        let end_row = first + wrapped.len().saturating_sub(1);
        Some(self.first_non_blank(self.clamped(self.at_row(end_row, false), false)))
    }

    /// Rewrites the indentation of rows `first..=last` to what
    /// [`indent::reindent`] says the structure wants — `=`. The lines above
    /// the range are context: walked for their bracket depth, never touched.
    /// Says where the cursor goes — the first non-blank of the last row
    /// touched. `None` when every line already sat where it should.
    pub fn reindent_rows(
        &mut self,
        first: usize,
        last: usize,
        indent: &Indent,
    ) -> Option<Cursor> {
        let last = last.min(self.line_count().saturating_sub(1));
        let lines: Vec<String> = (0..=last).map(|row| self.line(row)).collect();
        let new = indent::reindent(&lines, first, indent);
        let mut changed = false;

        // Bottom-up, so an edit on one row cannot move the rows still to come.
        for row in (first..=last).rev() {
            let text = &lines[row];
            let lead = indent::leading(text);
            let wanted = &new[row - first];
            if lead == wanted {
                continue;
            }
            let start = self.rope.line_to_char(row);
            self.apply_edit(start, start + lead.chars().count(), wanted);
            changed = true;
        }

        changed.then(|| self.first_non_blank(self.clamped(self.at_row(last, false), false)))
    }

    /// `Tab` in insert mode: forward to the next indent stop.
    ///
    /// Aligns rather than inserting a fixed width, which is what a tab has
    /// always meant on a terminal — at column 6 with a step of 4 it moves two
    /// columns, so the line below lines up with the one above it.
    ///
    /// A literal tab only when the file is written with tabs *and* one lands
    /// exactly on the stop: at `tab_width = 8` with `shiftwidth = 4` a tab
    /// would overshoot by four columns, so spaces it is.
    pub fn insert_indent(&mut self, at: Cursor, indent: &Indent) -> Cursor {
        let text = self.line_text(self.row_at(at));
        let col = indent::display_col(&text, self.col_at(at), indent.tab_width);
        let step = indent.step();
        let width = step - (col % step);
        let tab = indent.tab_width.max(1);
        let one_tab = !indent.expandtab && tab - (col % tab) == width;
        let text = if one_tab { "\t".to_string() } else { " ".repeat(width) };
        self.insert_str(at, &text)
    }

    /// `Shift-Tab` in insert mode: back one indent stop.
    ///
    /// Only through whitespace. With anything else behind the cursor it does
    /// nothing at all rather than deleting a character that was typed.
    pub fn remove_indent(&mut self, at: Cursor, indent: &Indent) -> Cursor {
        let row = self.row_at(at);
        let col = self.col_at(at);
        let before: String = self.line_text(row).chars().take(col).collect();
        if before.is_empty() || !indent::is_blank(&before) {
            return at;
        }
        let step = indent.step();
        // The previous stop, and strictly before where we are: from column 4
        // with a step of 4 that is 0, not 4.
        let width = indent::width_of(&before, indent.tab_width);
        let width = (width.saturating_sub(1) / step) * step;
        let new = indent.render(width);
        let start = self.rope.line_to_char(row);
        self.apply_edit(start, start + col, &new);
        Cursor::at(start + new.chars().count())
    }

    /// `Backspace` in insert mode, which takes a whole indent when there is
    /// nothing but whitespace to the left of it on the line.
    ///
    /// With `expandtab` that is the difference between one press and four.
    /// Anywhere else on the line it is the plain backspace it always was.
    pub fn backspace_indent(&mut self, at: Cursor, indent: &Indent) -> Cursor {
        let col = self.col_at(at);
        if col > 0 {
            let before: String = self.line_text(self.row_at(at)).chars().take(col).collect();
            if indent::is_blank(&before) {
                return self.remove_indent(at, indent);
            }
        }
        self.backspace(at)
    }

    /// The text of `row`, without its terminator.
    ///
    /// Public for the decoration providers, which read lines and produce
    /// nothing that can change one — the rope stays private and every mutation
    /// still goes through `apply_edit`.
    pub fn line(&self, row: usize) -> String {
        self.line_text(row)
    }

    /// How far `row` is indented, in columns.
    ///
    /// Columns rather than characters, because a tab is worth `tab_width` of
    /// them and the answer is for something being drawn.
    pub fn indent_width(&self, row: usize, tab_width: usize) -> usize {
        let text = self.line_text(row);
        indent::width_of(indent::leading(&text), tab_width)
    }

    /// Whether `row` has nothing but whitespace on it. An empty row qualifies.
    pub fn is_blank_row(&self, row: usize) -> bool {
        indent::is_blank(&self.line_text(row))
    }

    /// Whether `row` actually ends in a line terminator.
    ///
    /// False for the last row of a file that does not end in one, which is the
    /// only place the two ever differ and is exactly what `:whitespace` is for:
    /// a missing final newline is invisible in every other way.
    pub fn has_newline(&self, row: usize) -> bool {
        row < self.rope.len_lines() && self.rope.line(row).len_chars() > self.line_len(row)
    }

    // ---- trimming ----------------------------------------------------------

    /// Tidies the text on its way to disk, and hands back the edits it made so
    /// the caller can carry its cursors across them.
    ///
    /// Edits rather than a rewritten rope: a cursor on line 400 must not move
    /// because three blank lines went from the top of the file, and only a
    /// mapping can promise that. It is also what keeps the reparse
    /// proportional to what changed. See `docs/specs/trim.md`.
    pub fn trim(&mut self, trim: &Trim) -> Vec<Edit> {
        let base = self.pending_edits.len();

        // Trailing first, so a line of nothing but spaces has become empty by
        // the time the two below ask whether it is blank.
        if trim.trailing {
            // Bottom-up: an edit on one row must not move the rows still to
            // come.
            for row in (0..self.line_count()).rev() {
                let text = self.line_text(row);
                let spare = crate::trim::trailing(&text);
                if spare == 0 {
                    continue;
                }
                let end = self.rope.line_to_char(row) + text.chars().count();
                self.apply_edit(end - spare, end, "");
            }
        }

        if trim.last_line {
            let rows = self.line_count();
            let keep = (0..rows).rev().find(|&row| !indent::is_blank(&self.line_text(row)));
            if let Some(keep) = keep
                && keep + 1 < rows
            {
                self.apply_edit(self.rope.line_to_char(keep + 1), self.rope.len_chars(), "");
            }
        }

        if trim.first_line {
            let rows = self.line_count();
            let blank = (0..rows).take_while(|&row| indent::is_blank(&self.line_text(row))).count();
            // A file of nothing but blank lines is left alone: there would be
            // nothing left of it, and "your file is now empty" is a bad thing
            // to learn from a write.
            if blank > 0 && blank < rows {
                self.apply_edit(0, self.rope.line_to_char(blank), "");
            }
        }

        // An empty file stays empty: a newline is a line terminator, and there
        // is no line here to terminate.
        if trim.final_newline && self.rope.len_chars() > 0 && !self.ends_with_newline() {
            let end = self.rope.len_chars();
            self.apply_edit(end, end, "\n");
        }

        self.pending_edits[base..].to_vec()
    }

    /// Rewrites the indentation of rows `first..=last` the way `indent` asks,
    /// and hands back how many lines changed and the edits that changed them.
    ///
    /// The edits are returned for the same reason [`Buffer::trim`] returns
    /// them: retabbing line 3 moves every offset below it, and a cursor on
    /// line 400 has to be carried across rather than clamped or reset.
    ///
    /// Beside `trim` rather than up in the editor because it is the same shape
    /// — a bulk rewrite of leading whitespace — and because a frontend that is
    /// not this one should be able to offer it without reimplementing the
    /// walk. The policy itself is [`indent::retab`]'s; this is the rope.
    pub fn retab(&mut self, first: usize, last: usize, indent: &Indent) -> (usize, Vec<Edit>) {
        let base = self.pending_edits.len();
        let mut lines = 0;

        // Bottom-up, so an edit on one row cannot move the rows still to come
        // — the same reason `trim` counts down.
        let last = last.min(self.line_count().saturating_sub(1));
        for row in (first..=last).rev() {
            let text = self.line_text(row);
            let Some(now) = indent::retab(&text, indent) else { continue };
            let start = self.rope.line_to_char(row);
            let was = indent::leading(&text).chars().count();
            self.apply_edit(start, start + was, &now);
            lines += 1;
        }

        (lines, self.pending_edits[base..].to_vec())
    }

    /// Clears a line that is nothing but whitespace, on the way out of insert
    /// mode.
    ///
    /// Without this, every line you opened and thought better of leaves an
    /// invisible indent behind — invisible on screen and perfectly visible in
    /// the diff. Returns where the cursor goes; unchanged when there was
    /// nothing to clear.
    pub fn clear_blank_line(&mut self, at: Cursor) -> Cursor {
        let row = self.row_at(at);
        let text = self.line_text(row);
        if text.is_empty() || !indent::is_blank(&text) {
            return at;
        }
        let start = self.rope.line_to_char(row);
        self.apply_edit(start, start + text.chars().count(), "");
        Cursor::at(start)
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

    /// The nearest position `cur` may legally hold.
    ///
    /// Also the row: a file ending in a newline has a phantom last line that
    /// [`Buffer::line_count`] does not count, and normal mode must not come to
    /// rest on it. `dd` on the last line used to land there — the row is one
    /// past the last one there is, and every linewise operator after it works
    /// out a range that starts below where it ends. Insert mode keeps the
    /// phantom, which is the line `o` on the last row is about to make real.
    pub fn clamped(&self, cur: Cursor, allow_eol: bool) -> Cursor {
        let at = cur.at.min(self.rope.len_chars());
        let row = match allow_eol {
            true => self.row_of(at),
            false => self.row_of(at).min(self.line_count().saturating_sub(1)),
        };
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
    pub fn word_forward(&self, cur: Cursor, allow_eol: bool, big: bool) -> Cursor {
        let len = self.rope.len_chars();
        let mut i = cur.at;
        if i >= len {
            return cur;
        }
        let start_class = self.class_at(i, big);
        if start_class != CharClass::Whitespace {
            while i < len && self.class_at(i, big) == start_class {
                i += 1;
            }
        }
        while i < len && self.class_at(i, big) == CharClass::Whitespace {
            i += 1;
        }
        let landed = self.clamped(Cursor::at(i.min(len)), allow_eol);
        // A file ending in a newline has a phantom row after it, and `clamped`
        // is happy to sit on it. Vim puts the cursor on the last real character
        // instead, which is what makes `w` on the final word of a file move at
        // all. Only in normal mode: an operator resolves with `allow_eol` and
        // needs to reach one past the end so `dw` can take the last word whole.
        if !allow_eol && landed.at >= len && len > 0 {
            return self.clamped(Cursor::at(len - 1), false);
        }
        landed
    }

    /// `b` / `B` — start of the previous word.
    pub fn word_backward(&self, cur: Cursor, allow_eol: bool, big: bool) -> Cursor {
        if cur.at == 0 {
            return cur;
        }
        let mut i = cur.at - 1;
        while i > 0 && self.class_at(i, big) == CharClass::Whitespace {
            i -= 1;
        }
        let class = self.class_at(i, big);
        if class != CharClass::Whitespace {
            while i > 0 && self.class_at(i - 1, big) == class {
                i -= 1;
            }
        }
        self.clamped(Cursor::at(i), allow_eol)
    }

    /// `e` / `E` / `ge` / `gE` — the end of a word rather than its start.
    ///
    /// Always moves at least one character before looking, or `e` sitting on
    /// the last character of a word would find that same character again and
    /// the motion would never advance.
    pub fn word_end(&self, cur: Cursor, allow_eol: bool, big: bool, forward: bool) -> Cursor {
        let len = self.rope.len_chars();
        if len == 0 {
            return cur;
        }
        let mut i = cur.at;
        if forward {
            if i + 1 >= len {
                return self.clamped(Cursor::at(len - 1), allow_eol);
            }
            i += 1;
            while i < len && self.class_at(i, big) == CharClass::Whitespace {
                i += 1;
            }
            if i >= len {
                return self.clamped(Cursor::at(len - 1), allow_eol);
            }
            let class = self.class_at(i, big);
            while i + 1 < len && self.class_at(i + 1, big) == class {
                i += 1;
            }
        } else {
            if i == 0 {
                return cur;
            }
            i -= 1;
            // Step off the current word first: `ge` from inside a word means
            // the end of the *previous* one, not the character behind you.
            let here = self.class_at(cur.at, big);
            if here != CharClass::Whitespace {
                while i > 0 && self.class_at(i, big) == here {
                    i -= 1;
                }
            }
            while i > 0 && self.class_at(i, big) == CharClass::Whitespace {
                i -= 1;
            }
        }
        self.clamped(Cursor::at(i.min(len.saturating_sub(1))), allow_eol)
    }

    /// `^` — the first character on the line that is not a blank, or the last
    /// column when the whole line is blank.
    pub fn first_non_blank(&self, cur: Cursor) -> Cursor {
        let row = self.row_at(cur);
        let start = self.rope.line_to_char(row);
        let len = self.line_len(row);
        let mut i = 0;
        while i < len && self.rope.char(start + i).is_whitespace() {
            i += 1;
        }
        Cursor::at(start + i.min(len.saturating_sub(1)))
    }

    /// `g_` — the last character on the line that is not a blank.
    pub fn last_non_blank(&self, cur: Cursor, allow_eol: bool) -> Cursor {
        let row = self.row_at(cur);
        let start = self.rope.line_to_char(row);
        let len = self.line_len(row);
        let mut i = len;
        while i > 0 && self.rope.char(start + i - 1).is_whitespace() {
            i -= 1;
        }
        self.clamped(Cursor::at(start + i.saturating_sub(1)), allow_eol)
    }

    /// `%` — the bracket matching the first one at or after the cursor.
    ///
    /// Vim scans forward on the line for a bracket to start from, so `%`
    /// anywhere before an opening paren still works. Staying put when there is
    /// none leaves an operator with an empty range, which is the right answer.
    pub fn matching_bracket(&self, cur: Cursor, allow_eol: bool) -> Cursor {
        const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
        let len = self.rope.len_chars();
        let row = self.row_at(cur);
        let row_end = self.rope.line_to_char(row) + self.line_len(row);

        let mut from = cur.at;
        let (open, close, forward) = loop {
            if from >= row_end || from >= len {
                return cur;
            }
            let c = self.rope.char(from);
            if let Some(&(o, c2)) = PAIRS.iter().find(|(o, _)| *o == c) {
                break (o, c2, true);
            }
            if let Some(&(o, c2)) = PAIRS.iter().find(|(_, c2)| *c2 == c) {
                break (o, c2, false);
            }
            from += 1;
        };

        let (mine, theirs) = if forward { (open, close) } else { (close, open) };
        let mut depth = 0usize;
        let mut i = from;
        loop {
            let c = self.rope.char(i);
            if c == mine {
                depth += 1;
            } else if c == theirs {
                depth -= 1;
                if depth == 0 {
                    return self.clamped(Cursor::at(i), allow_eol);
                }
            }
            if forward {
                i += 1;
                if i >= len {
                    return cur;
                }
            } else {
                if i == 0 {
                    return cur;
                }
                i -= 1;
            }
        }
    }

    /// `{` / `}` — the next blank line in that direction, or the end of the
    /// buffer. Vim's paragraph boundary, which is a line with nothing on it.
    pub fn paragraph_move(&self, cur: Cursor, forward: bool, allow_eol: bool) -> Cursor {
        let last = self.line_count().saturating_sub(1);
        let mut row = self.row_at(cur);
        loop {
            if forward {
                if row >= last {
                    return self.clamped(Cursor::at(self.rope.len_chars()), allow_eol);
                }
                row += 1;
            } else {
                if row == 0 {
                    return Cursor::at(0);
                }
                row -= 1;
            }
            if self.line_len(row) == 0 {
                return self.clamped(Cursor::at(self.rope.line_to_char(row)), allow_eol);
            }
        }
    }

    /// A character's class, with `big` folding punctuation into words so that
    /// only whitespace separates a WORD.
    fn class_at(&self, at: usize, big: bool) -> CharClass {
        match class_of(self.rope.char(at)) {
            CharClass::Whitespace => CharClass::Whitespace,
            other if big => {
                let _ = other;
                CharClass::Word
            }
            other => other,
        }
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
            Motion::Found(at) => return self.clamped(Cursor::at(at), eol),
            // Consumes its own count, and a miss stays put — which is what
            // leaves an operator with an empty range and so nothing to do.
            Motion::FindChar { ch, forward, till, repeat } => {
                return self.find_char(from, ch, forward, till, repeat, count).unwrap_or(from);
            }
            _ => {}
        }
        let mut cur = from;
        for _ in 0..count {
            cur = match motion {
                Motion::Left => self.left(cur),
                Motion::Right => self.right(cur, eol),
                Motion::Up => self.vertical(cur, -1, eol),
                Motion::Down => self.vertical(cur, 1, eol),
                Motion::Word { big, forward: true, end: false } => self.word_forward(cur, eol, big),
                Motion::Word { big, forward: false, end: false } => {
                    self.word_backward(cur, eol, big)
                }
                Motion::Word { big, forward, end: true } => self.word_end(cur, eol, big, forward),
                Motion::LineStart => self.line_start(cur),
                Motion::FirstNonBlank => self.first_non_blank(cur),
                Motion::LastNonBlank => self.last_non_blank(cur, eol),
                Motion::MatchingBracket => self.matching_bracket(cur, eol),
                Motion::Paragraph { forward } => self.paragraph_move(cur, forward, eol),
                Motion::LineEnd => self.line_end(cur, eol),
                Motion::CurrentLine
                | Motion::FirstLine
                | Motion::LastLine
                | Motion::Line(_)
                | Motion::FindChar { .. }
                | Motion::RepeatFind { .. }
                | Motion::Search { .. }
                | Motion::Found(_) => cur,
            };
        }
        cur
    }

    /// End of the `count`-th word from `at`, or `None` if `at` is whitespace.
    ///
    /// This is `e`, and it exists only to serve the `cw` quirk below.
    fn word_end_from(&self, at: usize, count: usize, big: bool) -> Option<usize> {
        let len = self.rope.len_chars();
        if at >= len || self.class_at(at, big) == CharClass::Whitespace {
            return None;
        }
        let mut i = at;
        for _ in 0..count {
            while i < len && self.class_at(i, big) == CharClass::Whitespace {
                i += 1;
            }
            if i >= len {
                break;
            }
            let class = self.class_at(i, big);
            while i + 1 < len && self.class_at(i + 1, big) == class {
                i += 1;
            }
            i += 1;
        }
        Some(i.saturating_sub(1))
    }

    /// The inclusive row span a linewise motion covers.
    fn linewise_rows(&self, at: Cursor, motion: Motion, count: usize) -> (usize, usize) {
        let last_row = self.line_count().saturating_sub(1);
        // Clamped before anything is worked out from it, so the pair can never
        // come back inside out — a cursor past the last row would otherwise
        // give a first row below the last one and a range no rope will slice.
        let start_row = self.row_at(at).min(last_row);
        let target_row = match motion {
            Motion::CurrentLine => start_row + count.max(1) - 1,
            _ => self.row_of(self.motion_target(motion, count, at).at),
        };
        (start_row.min(target_row), start_row.max(target_row).min(last_row))
    }

    /// The char range `op` + `motion` covers, and whether it was linewise.
    ///
    /// `None` when the motion goes nowhere — `b` at the start of the buffer,
    /// say — so the caller can leave the text alone rather than record an empty
    /// edit in the undo history.
    fn operator_range(
        &self,
        at: Cursor,
        op: Operator,
        target: Target,
        count: usize,
    ) -> Option<(usize, usize)> {
        let count = count.max(1);
        let len = self.rope.len_chars();

        // A text object names its range outright, so none of the motion
        // machinery below applies to it.
        let motion = match target {
            Target::Motion(m) => m,
            Target::Object { object, around } => {
                let (start, end) = self.object_range(at, object, around)?;
                // A linewise object has to take its terminator too, or `dip`
                // leaves the empty line behind. `cip` keeps it, for the same
                // reason `cc` does: insert mode needs a line to sit on.
                let end = if target.kind() == Kind::Linewise
                    && op != Operator::Change
                    && end < len
                    && self.rope.char(end) == '\n'
                {
                    end + 1
                } else {
                    end
                };
                return (end > start).then_some((start, end));
            }
        };

        if motion.kind() == Kind::Linewise {
            let (first, last) = self.linewise_rows(at, motion, count);
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
            // the file keeps a stray empty line — but only when the buffer does
            // not already end in one. When it does, `end` *is* that newline and
            // taking another would swallow the terminator `dG` should leave.
            let ends_in_newline = len > 0 && self.rope.char(len - 1) == '\n';
            return Some(if end == len && content_start > 0 && !ends_in_newline {
                (content_start - 1, end)
            } else {
                (content_start, end)
            });
        }

        // A find that misses covers nothing. This cannot be read off the target
        // alone: `f` is inclusive, so a target still sitting on the cursor gets
        // widened by one below and would delete a character the find never
        // reached.
        if let Motion::FindChar { ch, forward, till, repeat } = motion
            && self.find_char(at, ch, forward, till, repeat, count).is_none()
        {
            return None;
        }

        // Vim quirk: `cw` on a non-blank is `ce` — it changes the word without
        // swallowing the whitespace after it. On whitespace it is plain `w`.
        if op == Operator::Change
            && let Motion::Word { big, forward: true, end: false } = motion
            && let Some(end) = self.word_end_from(at.at, count, big)
        {
            return Some((at.at, (end + 1).min(len)));
        }

        let target = self.motion_target(motion, count, at).at;
        let (lo, mut hi) = (at.at.min(target), at.at.max(target));
        if motion.kind() == Kind::Inclusive {
            hi = (hi + 1).min(len);
        }
        // Vim quirk: an exclusive motion that leaves the line stops at the end
        // of it, so `dw` on the last word of a line does not join the next one.
        if matches!(motion, Motion::Word { forward: true, end: false, .. })
            && self.row_of(hi) > self.row_of(lo)
        {
            let row = self.row_of(lo);
            hi = self.rope.line_to_char(row) + self.line_len(row);
        }
        (hi > lo).then_some((lo, hi))
    }

    /// Applies `op` over `motion`, returning the text it took, where the
    /// cursor lands, and the char range it covered *in the text as it was*.
    ///
    /// The caller decides where the text goes — the buffer knows nothing about
    /// registers, which is what makes `"_` a policy at the call site rather
    /// than a flag threaded through here. `None` means the motion covered
    /// nothing and the buffer is untouched.
    ///
    /// The range is what the yank flash lights up. It was already computed in
    /// here — it is what `take` copies or cuts — and handing it back beats the
    /// caller guessing it out of the entry, whose text for a linewise yank is
    /// not the same length as what was on screen.
    pub fn operate(
        &mut self,
        at: Cursor,
        op: Operator,
        target: Target,
        count: usize,
    ) -> Option<(Entry, Cursor, std::ops::Range<usize>)> {
        let (start, end) = self.operator_range(at, op, target, count)?;
        let linewise = target.kind() == Kind::Linewise;

        // A linewise entry is always whole lines ending in a newline, even when
        // it came from a final line that had none — otherwise pasting it could
        // not open a line. So capture the *content* span rather than the span
        // the operator is about to remove, which differs at the buffer's end.
        let text = match (linewise, target) {
            (true, Target::Motion(motion)) => {
                let (first, last) = self.linewise_rows(at, motion, count);
                let from = self.rope.line_to_char(first);
                let to = self.rope.line_to_char(last) + self.line_len(last);
                let mut text = self.rope.slice(from..to).to_string();
                text.push('\n');
                text
            }
            // A linewise object already knows its own span; it just has to end
            // in a newline like every other linewise entry, or pasting it back
            // could not open a line.
            (true, Target::Object { .. }) => {
                let mut text = self.rope.slice(start..end).to_string();
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text
            }
            (false, _) => self.rope.slice(start..end).to_string(),
        };
        Some(self.take(at, op, start, end, text, linewise))
    }

    /// Applies `op` over an explicit char range.
    ///
    /// What visual mode uses: the range comes from the selection rather than
    /// from a motion, so none of the target machinery applies. `Buffer` is kept
    /// out of the selection business entirely — `Editor` works out the range
    /// and hands it over.
    pub fn operate_range(
        &mut self,
        at: Cursor,
        op: Operator,
        start: usize,
        end: usize,
        linewise: bool,
    ) -> Option<(Entry, Cursor, std::ops::Range<usize>)> {
        if end <= start {
            return None;
        }
        let mut text = self.rope.slice(start..end).to_string();
        // Every linewise entry ends in a newline, or pasting it back could not
        // open a line.
        if linewise && !text.ends_with('\n') {
            text.push('\n');
        }
        Some(self.take(at, op, start, end, text, linewise))
    }

    /// The shared tail of both: capture `text`, then either leave the buffer
    /// alone (yank) or cut the range out.
    fn take(
        &mut self,
        at: Cursor,
        op: Operator,
        start: usize,
        end: usize,
        text: String,
        linewise: bool,
    ) -> (Entry, Cursor, std::ops::Range<usize>) {
        let kind = if linewise { Shape::Lines } else { Shape::Chars };

        let landed = if op == Operator::Yank {
            // Yank moves the cursor to the start of what it took, and nowhere
            // for the forward motions where that is already the cursor.
            if start < at.at { self.clamped(Cursor::at(start), false) } else { at }
        } else {
            self.apply_edit(start, end, "");
            // Delete lands back on a char; change leaves the cursor where the
            // text was, which for `d$` or `cc` is one past what's left.
            self.clamped(Cursor::at(start), op == Operator::Change)
        };
        (Entry { text, kind }, landed, start..end)
    }

    /// Moves rows `first..=last` so the block starts at `to`.
    ///
    /// `None` when there is nothing to do, so a `Shift-Up` on the first line
    /// puts no no-op on the undo stack. Returns where the cursor lands: the
    /// first row that moved, in the column it was already in, because anything
    /// else makes a held key walk away from the line it is pushing.
    ///
    /// See `docs/specs/move-lines.md`.
    pub fn move_lines(
        &mut self,
        first: usize,
        last: usize,
        to: usize,
        col: usize,
    ) -> Option<Cursor> {
        let lines = self.line_count();
        let last = last.min(lines.saturating_sub(1));
        if first > last {
            return None;
        }
        let height = last - first + 1;
        // Clamped rather than refused: `:m +99` near the bottom means "to the
        // bottom", which is what someone typing a big number wants.
        let to = to.min(lines - height);
        if to == first {
            return None;
        }

        let (from, upto) =
            self.line_range(self.rope.line_to_char(first), self.rope.line_to_char(last), true);
        let mut block = self.rope.slice(from..upto).to_string();
        let terminated = block.ends_with('\n');
        // Lifting the final line out takes the newline that ended the line
        // *above* it rather than inventing one at the end, so a file with no
        // terminator still has none afterwards.
        let cut_from = if terminated { from } else { from.saturating_sub(1) };
        if !terminated {
            block.push('\n');
        }

        self.apply_edit(cut_from, upto, "");

        // `to` is where the block should start in the finished text, and the
        // rope with the block gone is exactly the finished text without it —
        // so `to` is the insertion row, whichever way the block travelled.
        let at = match to < self.line_count() {
            true => self.rope.line_to_char(to),
            false => self.rope.len_chars(),
        };

        // Putting it back past a final line with no terminator would weld it
        // to the line above, so the newline moves to the front instead.
        let welds = at == self.rope.len_chars() && !self.ends_with_newline();
        let text = match welds {
            true => format!("\n{}", block.trim_end_matches('\n')),
            false => block,
        };
        self.apply_edit(at, at, &text);

        let landed = at + if welds { 1 } else { 0 };
        let row = self.row_at(Cursor::at(landed));
        Some(self.clamped(Cursor::at(self.rope.line_to_char(row) + col), false))
    }

    fn ends_with_newline(&self) -> bool {
        let len = self.rope.len_chars();
        len > 0 && self.rope.char(len - 1) == '\n'
    }

    /// Char range covering the whole of every line `start..=end` touches,
    /// plus the terminator when there is one. Linewise visual.
    pub fn line_range(&self, start: usize, end: usize, take_terminator: bool) -> (usize, usize) {
        let first = self.row_at(Cursor::at(start));
        let last = self.row_at(Cursor::at(end));
        let from = self.rope.line_to_char(first);
        let content_end = self.rope.line_to_char(last) + self.line_len(last);
        let len = self.rope.len_chars();
        let to = if take_terminator && content_end < len && self.rope.char(content_end) == '\n' {
            content_end + 1
        } else {
            content_end
        };
        (from, to)
    }

    /// Puts `entry` back, `count` times, as one edit.
    pub fn paste(&mut self, at: Cursor, entry: &Entry, before: bool, count: usize) -> Cursor {
        let count = count.max(1);
        match entry.kind {
            Shape::Chars => {
                let row = self.row_at(at);
                let line_end = self.rope.line_to_char(row) + self.line_len(row);
                // `p` goes after the char under the cursor — but an empty line
                // has no such char, so it must not step onto the next one.
                let start = if before { at.at } else { (at.at + 1).min(line_end) };
                let text = entry.text.repeat(count);
                let len = text.chars().count();
                self.apply_edit(start, start, &text);
                self.clamped(Cursor::at(start + len.saturating_sub(1)), false)
            }
            Shape::Lines => {
                let row = self.row_at(at);
                let start = if before {
                    self.rope.line_to_char(row)
                } else if row + 1 < self.rope.len_lines() {
                    self.rope.line_to_char(row + 1)
                } else {
                    self.rope.len_chars()
                };

                let mut text = entry.text.repeat(count);
                // Appending past a final line that has no newline: the entry's
                // trailing newline has to move to the front, or the text lands
                // on the end of that line instead of below it.
                if start == self.rope.len_chars() && start > 0 && self.rope.char(start - 1) != '\n'
                {
                    let body = text.strip_suffix('\n').unwrap_or(&text);
                    text = format!("\n{body}");
                }

                self.apply_edit(start, start, &text);
                let landed = if before { row } else { row + 1 };
                // Vim puts the cursor on the first *non-blank* of the pasted
                // line, which matters the moment you paste indented code.
                // `registers.md` settled for the first character until a
                // first-non-blank motion existed; `^` is now one.
                self.first_non_blank(self.at_row(landed, false))
            }
            Shape::Block => {
                let row = self.row_at(at);
                let col = self.col_at(at);
                // `p` goes after the char under the cursor, as charwise does.
                let col = if before { col } else { (col + 1).min(self.line_len(row)) };

                for (i, piece) in entry.text.split('\n').enumerate() {
                    let target = row + i;
                    // The block can reach past the last line; grow the buffer
                    // rather than piling every remaining row onto the end.
                    if target >= self.rope.len_lines() {
                        let end = self.rope.len_chars();
                        self.apply_edit(end, end, "\n");
                    }
                    // Recomputed per row: each insert shifts everything below
                    // it, so nothing may be cached across the loop.
                    let line_start = self.rope.line_to_char(target);
                    let len = self.line_len(target);
                    // A row shorter than the column is padded out to it, or
                    // the text would slide left and stop being a rectangle.
                    let pad = col.saturating_sub(len);
                    let text = format!("{}{}", " ".repeat(pad), piece.repeat(count));
                    let start = line_start + col.min(len);
                    self.apply_edit(start, start, &text);
                }

                self.clamped(Cursor::at(self.rope.line_to_char(row) + col), false)
            }
        }
    }

    /// Replaces `start..end` with `entry`, `count` times. Visual mode's `p`.
    ///
    /// One edit rather than a delete followed by a paste, because the two would
    /// disagree about where the text goes the moment the kinds differ: a
    /// linewise entry pasted after a charwise delete would open a line below
    /// the whole line, not where the selection was.
    ///
    /// Returns what it removed instead of capturing it — the same division
    /// `operate_range` draws, and what lets `P` drop it — along with where the
    /// cursor lands. `linewise` is the *selection's* kind; the entry carries
    /// its own. A blockwise selection is spans rather than a range and never
    /// reaches here; `Editor` walks those.
    ///
    /// See `docs/specs/registers.md`.
    pub fn paste_over(
        &mut self,
        start: usize,
        end: usize,
        linewise: bool,
        entry: &Entry,
        count: usize,
    ) -> (Entry, Cursor) {
        let count = count.max(1);
        let mut text = self.rope.slice(start..end).to_string();
        // Whether the range carried its line's terminator, which is what says
        // if the replacement may end in one.
        let terminated = text.ends_with('\n');
        if linewise && !terminated {
            text.push('\n');
        }
        let kind = if linewise { Shape::Lines } else { Shape::Chars };
        let removed = Entry { text, kind };

        let landed = match (linewise, entry.kind) {
            // A rectangle goes back where the range was, so the range goes
            // first and the block paste does the rest.
            (_, Shape::Block) => {
                self.apply_edit(start, end, "");
                let at = self.clamped(Cursor::at(start), true);
                self.paste(at, entry, true, count)
            }
            (false, Shape::Chars) => {
                let text = entry.text.repeat(count);
                let len = text.chars().count();
                self.apply_edit(start, end, &text);
                self.clamped(Cursor::at(start + len.saturating_sub(1)), false)
            }
            // Lines cannot sit inside a line, so the line splits where the
            // selection was and they land between the halves.
            (false, Shape::Lines) => {
                self.apply_edit(start, end, &format!("\n{}", entry.text.repeat(count)));
                let row = self.row_at(Cursor::at(start)) + 1;
                self.first_non_blank(self.at_row(row, false))
            }
            // Whole lines for whole lines. A charwise entry becomes a line of
            // its own; either way the trailing newline goes only where the
            // range took one, so a file that ended without one still does.
            (true, kind) => {
                let mut text = entry.text.repeat(count);
                if kind == Shape::Chars {
                    text.push('\n');
                }
                // One newline, not every trailing one: an entry of blank lines
                // is mostly newlines and they are its content.
                if !terminated && text.ends_with('\n') {
                    text.pop();
                }
                let row = self.row_at(Cursor::at(start));
                self.apply_edit(start, end, &text);
                self.first_non_blank(self.at_row(row, false))
            }
        };
        (removed, landed)
    }

    /// Where `motion` lands from `at`. Pure, like every other motion here —
    /// the buffer has no cursor of its own to move.
    pub fn moved(&self, at: Cursor, motion: Motion, allow_eol: bool) -> Cursor {
        match motion {
            Motion::Left => self.left(at),
            Motion::Right => self.right(at, allow_eol),
            Motion::Up => self.vertical(at, -1, allow_eol),
            Motion::Down => self.vertical(at, 1, allow_eol),
            Motion::Word { big, forward: true, end: false } => {
                self.word_forward(at, allow_eol, big)
            }
            Motion::Word { big, forward: false, end: false } => {
                self.word_backward(at, allow_eol, big)
            }
            Motion::Word { big, forward, end: true } => self.word_end(at, allow_eol, big, forward),
            Motion::LineStart => self.line_start(at),
            Motion::FirstNonBlank => self.first_non_blank(at),
            Motion::LastNonBlank => self.last_non_blank(at, allow_eol),
            Motion::MatchingBracket => self.matching_bracket(at, allow_eol),
            Motion::Paragraph { forward } => self.paragraph_move(at, forward, allow_eol),
            Motion::LineEnd => self.line_end(at, allow_eol),
            Motion::FirstLine => self.at_row(0, allow_eol),
            Motion::LastLine => self.at_row(usize::MAX, allow_eol),
            Motion::Line(n) => self.at_row(n.saturating_sub(1), allow_eol),
            Motion::Found(found) => self.clamped(Cursor::at(found), allow_eol),
            Motion::CurrentLine => at,
            Motion::FindChar { ch, forward, till, repeat } => {
                self.find_char(at, ch, forward, till, repeat, 1).unwrap_or(at)
            }
            // Both are substituted by `Editor` before they get here — the
            // pattern and the last find both live up there.
            Motion::RepeatFind { .. } | Motion::Search { .. } => at,
        }
    }

    // ---- find-char ---------------------------------------------------------

    /// Bounds of `row` as a char range, excluding the line terminator.
    fn line_span(&self, row: usize) -> (usize, usize) {
        let start = self.rope.line_to_char(row);
        (start, start + self.line_len(row))
    }

    /// `f` `F` `t` `T`. `None` when the character is not on the line, which is
    /// what makes `df;` on a line with no `;` change nothing.
    ///
    /// The count is consumed here rather than by repeating the motion, because
    /// `t` lands one short of its target: repeating it from there would find
    /// the same character again and never advance.
    fn find_char(
        &self,
        from: Cursor,
        ch: char,
        forward: bool,
        till: bool,
        repeat: bool,
        count: usize,
    ) -> Option<Cursor> {
        let (line_start, line_end) = self.line_span(self.row_of(from.at));
        let mut at = from.at;

        // A repeated `t` starts one character further along: the cursor is
        // already parked next to the match it found last time, so searching
        // from here would find that same one and never advance. A freshly typed
        // `t` must *not* do this — vim draws the same distinction through
        // `cpo`'s `;` flag, and `t.` then `;` is where you notice.
        if till && repeat {
            at =
                if forward { (at + 1).min(line_end) } else { at.saturating_sub(1).max(line_start) };
        }

        for _ in 0..count.max(1) {
            if forward {
                let mut i = at + 1;
                loop {
                    if i >= line_end {
                        return None;
                    }
                    if self.rope.char(i) == ch {
                        break;
                    }
                    i += 1;
                }
                at = i;
            } else {
                let mut i = at;
                loop {
                    if i <= line_start {
                        return None;
                    }
                    i -= 1;
                    if self.rope.char(i) == ch {
                        break;
                    }
                }
                at = i;
            }
        }

        // `t` stops one short. Guarded so `t` can never step outside the line.
        Some(Cursor::at(match (till, forward) {
            (true, true) => at.saturating_sub(1).max(line_start),
            (true, false) => (at + 1).min(line_end),
            (false, _) => at,
        }))
    }

    /// Next occurrence of `needle` after `from`, wrapping to the start.
    pub fn find_next(&self, from: usize, needle: &str) -> Option<usize> {
        self.search(from, needle, true, false)
    }

    /// Whether a match at `start` of `len` chars is bounded by non-word
    /// characters — what `*` needs so `foo` does not match inside `foobar`.
    fn is_whole_word(chars: &[char], start: usize, len: usize) -> bool {
        let before = start.checked_sub(1).map(|i| class_of(chars[i]));
        let after = chars.get(start + len).map(|&c| class_of(c));
        before != Some(CharClass::Word) && after != Some(CharClass::Word)
    }

    /// Next match of `needle` from `from`, in either direction, wrapping.
    ///
    /// Smartcase: an all-lowercase needle matches case-insensitively, and any
    /// uppercase in it makes the whole thing case-sensitive. That is
    /// `ignorecase` plus `smartcase`, which is what nearly everyone sets.
    ///
    /// Naive scan over a materialised `Vec<char>`. Fine at the sizes bi opens;
    /// a regex backend will replace the matching without changing the shape.
    pub fn search(
        &self,
        from: usize,
        needle: &str,
        forward: bool,
        whole_word: bool,
    ) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let cased = needle.chars().any(char::is_uppercase);
        let fold = |c: char| if cased { c } else { c.to_ascii_lowercase() };

        let chars: Vec<char> = self.rope.chars().map(fold).collect();
        let raw: Vec<char> = self.rope.chars().collect();
        let pat: Vec<char> = needle.chars().map(fold).collect();
        if pat.len() > chars.len() {
            return None;
        }

        let last = chars.len() - pat.len();
        let hit = |i: usize| {
            chars[i..i + pat.len()] == pat[..]
                && (!whole_word || Self::is_whole_word(&raw, i, pat.len()))
        };

        // Wrapping, so a cursor past the final match still finds the first one.
        for offset in 0..=last {
            let i = if forward {
                (from + 1 + offset) % (last + 1)
            } else {
                // Counting down from `from - 1`, wrapping to the end.
                (from + last + 1 - (offset + 1) % (last + 1)) % (last + 1)
            };
            if hit(i) {
                return Some(i);
            }
        }
        None
    }

    /// How many edits this buffer has seen. Only ever compared for equality:
    /// a cache built over the text is stale when the number has moved.
    pub fn edits(&self) -> u64 {
        self.edits
    }

    /// Where every match in the whole buffer starts, in order.
    ///
    /// Unbounded on purpose — the status line's `[3/17]` cannot be answered
    /// from the viewport. One pass, and `Editor` caches it against `edits()`
    /// so it runs when the text or the pattern changes rather than per frame.
    pub fn match_starts(&self, needle: &str, whole_word: bool) -> Vec<usize> {
        self.matches_in(0, self.rope.len_chars(), needle, whole_word)
            .into_iter()
            .map(|(start, _)| start)
            .collect()
    }

    /// Every match inside `start..end`, for the renderer to highlight.
    ///
    /// Takes a range so the search-highlight pass stays bounded by the
    /// viewport, like every other pass in `render`.
    pub fn matches_in(
        &self,
        start: usize,
        end: usize,
        needle: &str,
        whole_word: bool,
    ) -> Vec<(usize, usize)> {
        self.matches_in_cased(start, end, needle, whole_word, None)
    }

    /// The same, with the case rule handed in rather than derived.
    ///
    /// `None` is smartcase, which is what `/` and everything built on it want.
    /// `:s`'s `i` and `I` flags are the one caller that knows better — they
    /// exist for when the guess is wrong, which is the only reason vim's `\c`
    /// and `\C` exist either. See `docs/specs/substitute.md`.
    pub fn matches_in_cased(
        &self,
        start: usize,
        end: usize,
        needle: &str,
        whole_word: bool,
        case: Option<bool>,
    ) -> Vec<(usize, usize)> {
        if needle.is_empty() {
            return Vec::new();
        }
        let cased = case.unwrap_or_else(|| needle.chars().any(char::is_uppercase));
        let fold = |c: char| if cased { c } else { c.to_ascii_lowercase() };

        let raw: Vec<char> = self.rope.chars().collect();
        let chars: Vec<char> = raw.iter().map(|&c| fold(c)).collect();
        let pat: Vec<char> = needle.chars().map(fold).collect();
        if pat.len() > chars.len() {
            return Vec::new();
        }

        let mut out = Vec::new();
        let mut i = start.min(chars.len() - pat.len());
        while i + pat.len() <= chars.len() && i < end {
            if chars[i..i + pat.len()] == pat[..]
                && (!whole_word || Self::is_whole_word(&raw, i, pat.len()))
            {
                out.push((i, i + pat.len()));
                i += pat.len();
            } else {
                i += 1;
            }
        }
        out
    }

    /// The word under `at`, for `Ctrl-N` to look for.
    pub fn word_at(&self, at: Cursor) -> Option<(usize, usize)> {
        self.object_range(at, TextObject::Word { big: false }, false)
    }

    /// The character at `at`, or `None` past the end.
    pub fn char_at(&self, at: usize) -> Option<char> {
        (at < self.rope.len_chars()).then(|| self.rope.char(at))
    }

    /// Text of a char range.
    pub fn slice(&self, start: usize, end: usize) -> String {
        let len = self.rope.len_chars();
        self.rope.slice(start.min(len)..end.min(len)).to_string()
    }

    // ---- text objects ------------------------------------------------------

    /// The char range a text object covers, or `None` when the cursor is not
    /// inside one.
    ///
    /// Lives here for the same reason the motion resolvers do: it needs the
    /// rope. Returns a range rather than a cursor, which is the whole
    /// difference between an object and a motion.
    pub fn object_range(
        &self,
        at: Cursor,
        object: TextObject,
        around: bool,
    ) -> Option<(usize, usize)> {
        match object {
            TextObject::Word { big } => self.word_object(at, around, big),
            TextObject::Quoted(q) => self.quoted_object(at, q, around),
            TextObject::Delimited(open) => self.delimited_object(at, open, around),
            TextObject::Paragraph => self.paragraph_object(at, around),
        }
    }

    /// `iw` is the run of same-class characters under the cursor. `aw` adds the
    /// whitespace after it, or — when there is none, at the end of a line — the
    /// whitespace before it, which is what vim does.
    fn word_object(&self, at: Cursor, around: bool, big: bool) -> Option<(usize, usize)> {
        let len = self.rope.len_chars();
        let at = at.at;
        if at >= len {
            return None;
        }
        // A WORD is anything non-blank, so punctuation and letters are one run.
        let class = |c: char| {
            let k = class_of(c);
            if big && k == CharClass::Punct { CharClass::Word } else { k }
        };
        let here = class(self.rope.char(at));
        let (line_start, line_end) = self.line_span(self.row_of(at));

        let mut start = at;
        while start > line_start && class(self.rope.char(start - 1)) == here {
            start -= 1;
        }
        let mut end = at + 1;
        while end < line_end && class(self.rope.char(end)) == here {
            end += 1;
        }
        if !around {
            return Some((start, end));
        }

        let mut after = end;
        while after < line_end && class_of(self.rope.char(after)) == CharClass::Whitespace {
            after += 1;
        }
        if after > end {
            return Some((start, after));
        }
        // Nothing trailing to take, so reach backwards instead.
        let mut before = start;
        while before > line_start && class_of(self.rope.char(before - 1)) == CharClass::Whitespace {
            before -= 1;
        }
        Some((before, end))
    }

    /// `i"` — between the quotes; `a"` — including them.
    ///
    /// Quotes cannot nest, so this pairs them in order along the line. That is
    /// vim's rule, and it is why `ci"` behaves oddly on a line with an odd
    /// number of quotes. Preserved rather than improved on: a better rule wants
    /// the parse tree.
    fn quoted_object(&self, at: Cursor, quote: char, around: bool) -> Option<(usize, usize)> {
        let at = at.at;
        let (line_start, line_end) = self.line_span(self.row_of(at));

        let mut pairs = Vec::new();
        let mut open: Option<usize> = None;
        let mut i = line_start;
        while i < line_end {
            if self.rope.char(i) == quote {
                match open.take() {
                    Some(o) => pairs.push((o, i)),
                    None => open = Some(i),
                }
            }
            i += 1;
        }

        // The cursor may be on the opening quote, inside, or on the closer.
        let (o, c) = pairs.into_iter().find(|&(o, c)| at >= o && at <= c)?;
        if !around {
            return (c > o + 1).then_some((o + 1, c));
        }

        // `a"` reaches for the whitespace after the closing quote, and only
        // falls back to the whitespace before the opening one when there is
        // none — the same rule `aw` follows, and the reason `da"` leaves one
        // space rather than two.
        let end = c + 1;
        let mut after = end;
        while after < line_end && self.rope.char(after).is_whitespace() {
            after += 1;
        }
        if after > end {
            return Some((o, after));
        }
        let mut before = o;
        while before > line_start && self.rope.char(before - 1).is_whitespace() {
            before -= 1;
        }
        Some((before, end))
    }

    /// `i(` — inside the brackets; `a(` — including them.
    ///
    /// Counts nesting on the way out, or `di(` inside `f(g(x))` would find the
    /// wrong pair.
    fn delimited_object(&self, at: Cursor, open: char, around: bool) -> Option<(usize, usize)> {
        let close = match open {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '<' => '>',
            _ => return None,
        };
        let len = self.rope.len_chars();
        let at = at.at.min(len.saturating_sub(1));

        // Sitting on a bracket counts as being inside that pair.
        let start = if self.rope.char(at) == open {
            at
        } else {
            let mut depth = 0usize;
            let mut i = at;
            loop {
                let c = self.rope.char(i);
                if c == close && i != at {
                    depth += 1;
                } else if c == open {
                    if depth == 0 {
                        break i;
                    }
                    depth -= 1;
                }
                if i == 0 {
                    return None;
                }
                i -= 1;
            }
        };

        let mut depth = 0usize;
        let mut i = start + 1;
        let end = loop {
            if i >= len {
                return None;
            }
            let c = self.rope.char(i);
            if c == open {
                depth += 1;
            } else if c == close {
                if depth == 0 {
                    break i;
                }
                depth -= 1;
            }
            i += 1;
        };

        if around {
            return Some((start, end + 1));
        }
        if end <= start + 1 {
            return None;
        }

        // Vim: when the contents occupy whole lines, `i{` covers those lines
        // rather than the exact span — `di{` on a braced body leaves the braces
        // on their own lines instead of collapsing them to `{}`. Only applies
        // when nothing but whitespace shares the bracket lines.
        let open_row = self.row_of(start);
        let close_row = self.row_of(end);
        if close_row > open_row + 1 {
            let (_, open_line_end) = self.line_span(open_row);
            let close_line_start = self.rope.line_to_char(close_row);
            let tail_blank = (start + 1..open_line_end).all(|i| self.rope.char(i).is_whitespace());
            let head_blank = (close_line_start..end).all(|i| self.rope.char(i).is_whitespace());
            if tail_blank && head_blank {
                return Some((self.rope.line_to_char(open_row + 1), close_line_start));
            }
        }
        Some((start + 1, end))
    }

    /// `ip` — the run of non-blank lines around the cursor, or the run of blank
    /// ones when it sits on a blank line. `ap` adds the blank lines after.
    fn paragraph_object(&self, at: Cursor, around: bool) -> Option<(usize, usize)> {
        let last = self.line_count().saturating_sub(1);
        let blank = |row: usize| self.line_len(row) == 0;
        let here = blank(self.row_at(at));

        let mut first = self.row_at(at);
        while first > 0 && blank(first - 1) == here {
            first -= 1;
        }
        let mut end = self.row_at(at);
        while end < last && blank(end + 1) == here {
            end += 1;
        }
        if around {
            while end < last && blank(end + 1) != here {
                end += 1;
            }
        }

        let from = self.rope.line_to_char(first);
        let to = self.rope.line_to_char(end) + self.line_len(end);
        (to > from || first != end).then_some((from, to))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- moving lines -------------------------------------------------------

    fn rows(text: &str) -> Buffer {
        let mut buffer = Buffer::empty();
        buffer.insert_str(Cursor::at(0), text);
        buffer
    }

    fn shown(buffer: &Buffer) -> String {
        buffer.rope().to_string()
    }

    #[test]
    fn a_line_moves_down_and_the_cursor_rides_with_it() {
        let mut buffer = rows("alpha\nbeta\ngamma\ndelta\n");

        let landed = buffer.move_lines(0, 0, 2, 3).expect("moved");

        assert_eq!(shown(&buffer), "beta\ngamma\nalpha\ndelta\n");
        assert_eq!(buffer.row_at(landed), 2, "on the line it pushed");
        assert_eq!(buffer.col_at(landed), 3, "in the column it was in");
    }

    #[test]
    fn a_line_moves_up() {
        let mut buffer = rows("alpha\nbeta\ngamma\n");

        let landed = buffer.move_lines(2, 2, 0, 0).expect("moved");

        assert_eq!(shown(&buffer), "gamma\nalpha\nbeta\n");
        assert_eq!(buffer.row_at(landed), 0);
    }

    #[test]
    fn a_block_moves_as_one() {
        let mut buffer = rows("a\nb\nc\nd\ne\n");

        buffer.move_lines(0, 1, 2, 0).expect("moved");

        assert_eq!(shown(&buffer), "c\nd\na\nb\ne\n");
    }

    /// `:m +99` near the bottom means "to the bottom". Refusing would be
    /// correct and useless.
    #[test]
    fn a_move_past_the_end_clamps_rather_than_refusing() {
        let mut buffer = rows("a\nb\nc\n");

        buffer.move_lines(0, 0, 99, 0).expect("moved");

        assert_eq!(shown(&buffer), "b\nc\na\n");
    }

    #[test]
    fn moving_a_block_onto_itself_is_not_an_edit() {
        let mut buffer = rows("a\nb\nc\n");
        let before = buffer.edits();

        assert!(buffer.move_lines(0, 0, 0, 0).is_none(), "nowhere to go");

        assert_eq!(buffer.edits(), before, "and nothing on the undo stack");
    }

    /// Where the rope arithmetic is: lifting the last line out takes a
    /// terminator it never had, and putting one back would weld it on.
    #[test]
    fn a_file_with_no_final_newline_survives_a_move_to_the_end() {
        let mut buffer = rows("alpha\nbeta\ngamma");

        buffer.move_lines(0, 0, 2, 0).expect("moved");

        assert_eq!(shown(&buffer), "beta\ngamma\nalpha");
    }

    #[test]
    fn a_file_with_no_final_newline_survives_its_last_line_moving_up() {
        let mut buffer = rows("alpha\nbeta\ngamma");

        buffer.move_lines(2, 2, 0, 0).expect("moved");

        assert_eq!(shown(&buffer), "gamma\nalpha\nbeta");
    }

    // ---- indentation --------------------------------------------------------

    fn spaces() -> Indent {
        Indent::default()
    }

    fn tabs() -> Indent {
        Indent { expandtab: false, ..Indent::default() }
    }

    #[test]
    fn indent_moves_a_line_one_step_each_way() {
        let mut buffer = rows("alpha\nbeta\n");

        buffer.indent_rows(0, 0, true, 1, &spaces()).expect("indented");
        assert_eq!(shown(&buffer), "    alpha\nbeta\n");

        buffer.indent_rows(0, 0, false, 1, &spaces()).expect("outdented");
        assert_eq!(shown(&buffer), "alpha\nbeta\n");
    }

    #[test]
    fn indent_writes_the_character_the_options_ask_for() {
        let mut buffer = rows("alpha\n");
        buffer.indent_rows(0, 0, true, 1, &tabs()).expect("indented");
        assert_eq!(shown(&buffer), "\talpha\n");

        // And converts what is already there, because the width is what is
        // being changed and the options say how a width is written.
        buffer.indent_rows(0, 0, true, 1, &spaces()).expect("indented");
        assert_eq!(shown(&buffer), "        alpha\n");
    }

    #[test]
    fn outdent_subtracts_a_step_rather_than_rounding_to_one() {
        let mut buffer = rows("      alpha\n");

        buffer.indent_rows(0, 0, false, 1, &spaces()).expect("outdented");

        assert_eq!(shown(&buffer), "  alpha\n", "6 columns, step 4, lands on 2 — not on 4");
    }

    #[test]
    fn outdent_clamps_at_column_zero_rather_than_eating_text() {
        let mut buffer = rows("  alpha\n");

        buffer.indent_rows(0, 0, false, 1, &spaces()).expect("outdented");
        assert_eq!(shown(&buffer), "alpha\n");
        assert!(buffer.indent_rows(0, 0, false, 1, &spaces()).is_none(), "nothing left to take");
    }

    #[test]
    fn a_step_counts_columns_not_characters() {
        // One tab and one space is five columns at tab_width 4, so one step
        // right is nine and one step left is one.
        let mut buffer = rows("\t alpha\n");

        buffer.indent_rows(0, 0, false, 1, &spaces()).expect("outdented");

        assert_eq!(shown(&buffer), " alpha\n");
    }

    #[test]
    fn a_blank_line_is_not_indented_but_is_emptied() {
        let mut buffer = rows("alpha\n\n   \nbeta\n");

        buffer.indent_rows(0, 3, true, 1, &spaces()).expect("indented");
        assert_eq!(shown(&buffer), "    alpha\n\n   \n    beta\n", "no indent goes onto nothing");

        buffer.indent_rows(0, 3, false, 1, &spaces()).expect("outdented");
        assert_eq!(shown(&buffer), "alpha\n\n\nbeta\n", "but `<` clears one out");
    }

    #[test]
    fn indent_lands_on_the_first_non_blank_of_the_first_row() {
        let mut buffer = rows("alpha\nbeta\n");

        let landed = buffer.indent_rows(0, 1, true, 1, &spaces()).expect("indented");

        assert_eq!(buffer.row_at(landed), 0);
        assert_eq!(buffer.col_at(landed), 4, "on the `a`, not in the whitespace");
    }

    #[test]
    fn several_steps_at_once() {
        let mut buffer = rows("alpha\n");

        buffer.indent_rows(0, 0, true, 3, &spaces()).expect("indented");

        assert_eq!(shown(&buffer), "            alpha\n");
    }

    #[test]
    fn tab_in_insert_mode_aligns_to_the_next_stop() {
        let mut buffer = rows("ab\n");

        // Two characters in, so the next stop is two columns away — not four.
        let landed = buffer.insert_indent(Cursor::at(2), &spaces());

        assert_eq!(shown(&buffer), "ab  \n");
        assert_eq!(landed.at, 4);
    }

    #[test]
    fn tab_writes_a_tab_only_when_one_lands_on_the_stop() {
        let mut buffer = rows("\n");
        buffer.insert_indent(Cursor::at(0), &tabs());
        assert_eq!(shown(&buffer), "\t\n");

        // A step of 4 under a tab width of 8: a tab would overshoot by four
        // columns, so spaces it is.
        let mut buffer = rows("\n");
        buffer.insert_indent(Cursor::at(0), &Indent { tab_width: 8, shiftwidth: 4, ..tabs() });
        assert_eq!(shown(&buffer), "    \n");
    }

    #[test]
    fn shift_tab_goes_back_a_stop_and_never_eats_text() {
        let mut buffer = rows("      alpha\n");
        let landed = buffer.remove_indent(Cursor::at(6), &spaces());
        assert_eq!(shown(&buffer), "    alpha\n");
        assert_eq!(landed.at, 4);

        // Behind text it does nothing at all.
        let mut buffer = rows("alpha\n");
        assert_eq!(buffer.remove_indent(Cursor::at(3), &spaces()).at, 3);
        assert_eq!(shown(&buffer), "alpha\n");
    }

    #[test]
    fn backspace_takes_a_whole_indent_and_then_a_character() {
        let mut buffer = rows("        alpha\n");

        let at = buffer.backspace_indent(Cursor::at(8), &spaces());
        assert_eq!(shown(&buffer), "    alpha\n");

        // Past the indent it is the backspace it always was.
        let at = buffer.backspace_indent(Cursor::at(at.at + 5), &spaces());
        assert_eq!(shown(&buffer), "    alph\n");
        assert_eq!(at.at, 8);
    }

    #[test]
    fn opening_a_line_copies_the_indent_as_it_is_written() {
        let mut buffer = rows("\talpha\n");

        let landed = buffer.open_line(Cursor::at(3), true, &tabs());

        assert_eq!(shown(&buffer), "\talpha\n\t\n", "a tab file stays a tab file");
        assert_eq!(buffer.row_at(landed), 1);
        assert_eq!(buffer.col_at(landed), 1);
    }

    #[test]
    fn opening_a_line_above_puts_the_indent_on_the_new_line() {
        let mut buffer = rows("    alpha\n");

        let landed = buffer.open_line(Cursor::at(4), false, &spaces());

        assert_eq!(shown(&buffer), "    \n    alpha\n");
        assert_eq!(buffer.row_at(landed), 0);
        assert_eq!(buffer.col_at(landed), 4);
    }

    #[test]
    fn autoindent_off_opens_at_column_zero() {
        let mut buffer = rows("    alpha\n");

        buffer.open_line(Cursor::at(4), true, &Indent { autoindent: false, ..spaces() });

        assert_eq!(shown(&buffer), "    alpha\n\n");
    }

    #[test]
    fn newline_indents_what_it_pushed_down() {
        let mut buffer = rows("    alpha beta\n");

        buffer.insert_newline(Cursor::at(10), &spaces());

        assert_eq!(shown(&buffer), "    alpha \n    beta\n");
    }

    #[test]
    fn a_whitespace_only_line_is_cleared_and_nothing_else_is() {
        let mut buffer = rows("alpha\n    \nbeta\n");

        let landed = buffer.clear_blank_line(Cursor::at(10));
        assert_eq!(shown(&buffer), "alpha\n\nbeta\n");
        assert_eq!(landed.at, 6);

        // A line with something on it is left alone, trailing space and all.
        let mut buffer = rows("alpha   \n");
        assert_eq!(buffer.clear_blank_line(Cursor::at(8)).at, 8);
        assert_eq!(shown(&buffer), "alpha   \n");
    }

    // ---- trimming -----------------------------------------------------------

    fn trimmed(text: &str, trim: Trim) -> String {
        let mut buffer = rows(text);
        buffer.trim(&trim);
        shown(&buffer)
    }

    #[test]
    fn trailing_whitespace_goes_from_every_line() {
        assert_eq!(
            trimmed("a  \nb\t\n   \nc\n", Trim::default()),
            "a\nb\n\nc\n",
            "and a line of nothing but whitespace becomes empty"
        );
    }

    #[test]
    fn blank_lines_go_from_both_ends_of_the_file() {
        let default = Trim::default();
        assert_eq!(trimmed("\n\na\n\n\n", default), "a\n");
        assert_eq!(trimmed("\n\na\n\n\n", Trim { last_line: false, ..default }), "a\n\n\n");
    }

    #[test]
    fn a_file_of_nothing_but_blank_lines_is_left_alone() {
        // There would be nothing left of it, and "your file is now empty" is a
        // bad thing to learn from a write.
        assert_eq!(trimmed("\n\n\n", Trim { last_line: true, ..Trim::default() }), "\n\n\n");
    }

    #[test]
    fn a_final_newline_is_added_and_never_doubled() {
        let trim = Trim { final_newline: true, ..Trim::default() };
        assert_eq!(trimmed("a", trim), "a\n");
        assert_eq!(trimmed("a\n", trim), "a\n");
        assert_eq!(trimmed("", trim), "", "an empty file has no line to terminate");
    }

    #[test]
    fn each_switch_is_read_on_its_own() {
        let nothing =
            Trim { trailing: false, first_line: false, last_line: false, ..Trim::default() };
        assert_eq!(trimmed("\n a  \n\n", nothing), "\n a  \n\n");
        assert_eq!(trimmed("\n a  \n\n", Trim { trailing: true, ..nothing }), "\n a\n\n");
        assert_eq!(trimmed("\n a  \n\n", Trim { first_line: true, ..nothing }), " a  \n\n");
    }

    #[test]
    fn trimming_hands_back_the_edits_so_a_cursor_can_follow() {
        let mut buffer = rows("\n\nalpha  \nbeta\n");

        let edits = buffer.trim(&Trim::default());

        // `beta` started at 10, and the two blank lines and two spaces before
        // it are gone.
        let at = edits.iter().fold(10, |at, edit| edit.map(at));
        assert_eq!(shown(&buffer), "alpha\nbeta\n");
        assert_eq!(buffer.rope().slice(at..at + 4).to_string(), "beta");
    }

    #[test]
    fn nothing_to_trim_is_no_edit_at_all() {
        let mut buffer = rows("alpha\nbeta\n");
        assert!(buffer.trim(&Trim::default()).is_empty(), "and so no undo entry either");
    }

    /// A buffer with a cursor attached.
    ///
    /// The cursor lives on `Editor` now, as a selection. These cases are about
    /// what the text does, not about threading a position through every call,
    /// so the fixture puts the old ergonomics back and derefs to `Buffer` for
    /// everything that did not need one.
    struct Fixture {
        inner: Buffer,
        cursor: Cursor,
        /// Where the cursor was when the open undo group started, mirroring
        /// `Editor::undo_from`. Without it a group records where it *ended*
        /// as its "before", and undo lands in the wrong place.
        undo_from: Option<Cursor>,
    }

    impl std::ops::Deref for Fixture {
        type Target = Buffer;
        fn deref(&self) -> &Buffer {
            &self.inner
        }
    }

    impl std::ops::DerefMut for Fixture {
        fn deref_mut(&mut self) -> &mut Buffer {
            &mut self.inner
        }
    }

    impl Fixture {
        fn cursor_row(&self) -> usize {
            self.inner.row_at(self.cursor)
        }
        fn cursor_col(&self) -> usize {
            self.inner.col_at(self.cursor)
        }
        fn apply_motion(&mut self, motion: Motion, eol: bool) {
            self.cursor = self.inner.moved(self.cursor, motion, eol);
        }
        fn goto_row(&mut self, row: usize, eol: bool) {
            self.cursor = self.inner.at_row(row, eol);
        }
        fn insert_str(&mut self, text: &str) {
            self.mark();
            self.cursor = self.inner.insert_str(self.cursor, text);
        }
        fn backspace(&mut self) {
            self.mark();
            self.cursor = self.inner.backspace(self.cursor);
        }
        fn open_line(&mut self, below: bool) {
            self.mark();
            // No autoindent: these tests are about where the line lands, and
            // the indent tests below say what the indent does.
            let plain = Indent { autoindent: false, ..Indent::default() };
            self.cursor = self.inner.open_line(self.cursor, below, &plain);
        }
        /// Single-cursor, so each "set" is one pair. Multi-cursor undo is
        /// covered in `editor.rs`.
        fn pairs(&self) -> crate::history::Cursors {
            vec![(self.cursor.at, self.cursor.at)]
        }
        /// Opens the undo group if it is not open already.
        fn mark(&mut self) {
            self.undo_from.get_or_insert(self.cursor);
        }
        fn bounds(&mut self) -> (crate::history::Cursors, crate::history::Cursors) {
            let from = self.undo_from.take().unwrap_or(self.cursor);
            (vec![(from.at, from.at)], self.pairs())
        }
        fn commit_undo(&mut self) {
            let (before, after) = self.bounds();
            self.inner.commit_undo(before, after);
        }
        fn undo(&mut self) -> bool {
            let (before, after) = self.bounds();
            match self.inner.undo(before, after) {
                Some(cursors) => {
                    self.cursor = Cursor::at(cursors.first().map_or(0, |&(_, h)| h));
                    true
                }
                None => false,
            }
        }
        fn redo(&mut self) -> bool {
            let (before, after) = self.bounds();
            match self.inner.redo(before, after) {
                Some(cursors) => {
                    self.cursor = Cursor::at(cursors.first().map_or(0, |&(_, h)| h));
                    true
                }
                None => false,
            }
        }
        fn operate(&mut self, op: Operator, target: Target, count: usize) -> Option<Entry> {
            self.mark();
            let (entry, landed, _) = self.inner.operate(self.cursor, op, target, count)?;
            self.cursor = landed;
            Some(entry)
        }
        fn paste(&mut self, entry: &Entry, before: bool, count: usize) {
            self.mark();
            self.cursor = self.inner.paste(self.cursor, entry, before, count);
        }
        fn replace_chars(&mut self, ch: char, count: usize) -> bool {
            self.mark();
            match self.inner.replace_chars(self.cursor, ch, count) {
                Some(c) => {
                    self.cursor = c;
                    true
                }
                None => false,
            }
        }
        fn toggle_case(&mut self, count: usize) {
            self.mark();
            self.cursor = self.inner.toggle_case(self.cursor, count);
        }
        fn join_lines(&mut self, count: usize) {
            self.mark();
            self.cursor = self.inner.join_lines(self.cursor, count);
        }
        fn object_range(&self, object: TextObject, around: bool) -> Option<(usize, usize)> {
            self.inner.object_range(self.cursor, object, around)
        }
        fn save_as(&mut self, path: impl AsRef<Path>) -> Result<()> {
            let (before, after) = self.bounds();
            self.inner.save_as(before, after, path)
        }
    }

    /// Loads `text` the way `open` does — straight into the rope, so the
    /// fixture leaves no undo history or pending edits behind.
    fn buf(text: &str) -> Fixture {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str(text);
        Fixture { inner: b, cursor: Cursor::default(), undo_from: None }
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
        b.apply_motion(Motion::Word { big: false, forward: true, end: false }, false);
        assert_eq!(b.cursor.at, 3, "stops at the dot");
        b.apply_motion(Motion::Word { big: false, forward: true, end: false }, false);
        assert_eq!(b.cursor.at, 4, "then the word after it");
        b.apply_motion(Motion::Word { big: false, forward: true, end: false }, false);
        assert_eq!(b.cursor.at, 9, "skips the double space");
        b.apply_motion(Motion::Word { big: false, forward: false, end: false }, false);
        assert_eq!(b.cursor.at, 4);
    }

    /// Shorthands, because the flags read badly inline.
    const fn word(forward: bool, end: bool) -> Motion {
        Motion::Word { big: false, forward, end }
    }
    const fn big_word(forward: bool, end: bool) -> Motion {
        Motion::Word { big: true, forward, end }
    }

    /// A WORD is whitespace-delimited, so `W` steps over the punctuation `w`
    /// stops at.
    #[test]
    fn big_word_motions_ignore_punctuation() {
        let mut b = buf("foo.bar  baz");
        b.apply_motion(big_word(true, false), false);
        assert_eq!(b.cursor.at, 9, "straight past the dot to the next WORD");
        b.apply_motion(big_word(false, false), false);
        assert_eq!(b.cursor.at, 0, "and all the way back");
    }

    #[test]
    fn e_lands_on_the_last_character_of_a_word_and_keeps_going() {
        let mut b = buf("foo.bar  baz");
        b.apply_motion(word(true, true), false);
        assert_eq!(b.cursor.at, 2, "the second o, not the dot");
        // From the end of a word it must advance, or `e` would never move.
        b.apply_motion(word(true, true), false);
        assert_eq!(b.cursor.at, 3, "the dot is its own word");
        b.apply_motion(word(true, true), false);
        assert_eq!(b.cursor.at, 6, "end of bar");

        let mut b = buf("foo.bar  baz");
        b.apply_motion(big_word(true, true), false);
        assert_eq!(b.cursor.at, 6, "E takes foo.bar whole");
    }

    #[test]
    fn ge_goes_back_to_the_end_of_the_previous_word() {
        let mut b = buf("foo bar baz");
        b.cursor = Cursor::at(9);
        b.apply_motion(word(false, true), false);
        assert_eq!(b.cursor.at, 6, "end of bar");
        b.apply_motion(word(false, true), false);
        assert_eq!(b.cursor.at, 2, "end of foo");
        b.apply_motion(word(false, true), false);
        assert_eq!(b.cursor.at, 0, "nowhere further back to go");
    }

    /// The difference between `de` and `dw` is the reason both keys exist:
    /// `e` is inclusive and stops at the word, `w` is exclusive and takes the
    /// whitespace after it.
    #[test]
    fn de_takes_the_word_and_dw_takes_the_space_too() {
        let mut b = buf("foo bar");
        b.operate(Operator::Delete, Target::Motion(word(true, true)), 1);
        assert_eq!(b.rope().to_string(), " bar");

        let mut b = buf("foo bar");
        b.operate(Operator::Delete, Target::Motion(word(true, false)), 1);
        assert_eq!(b.rope().to_string(), "bar");
    }

    #[test]
    fn caret_is_the_first_non_blank_and_zero_is_still_column_zero() {
        let mut b = buf("    indented");
        b.cursor = Cursor::at(8);
        b.apply_motion(Motion::FirstNonBlank, false);
        assert_eq!(b.cursor_col(), 4, "past the indent");
        b.apply_motion(Motion::LineStart, false);
        assert_eq!(b.cursor_col(), 0, "`0` is unchanged by `^` existing");

        // A line that is nothing but blanks has no non-blank to find.
        let mut b = buf("    ");
        b.apply_motion(Motion::FirstNonBlank, false);
        assert_eq!(b.cursor_col(), 3, "clamped rather than off the end");
    }

    #[test]
    fn g_underscore_is_the_last_non_blank() {
        let mut b = buf("hello world   ");
        b.apply_motion(Motion::LastNonBlank, false);
        assert_eq!(b.cursor.at, 10, "the d, not the trailing spaces");

        // Inclusive, like `$`, so the character it lands on comes with it.
        let mut b = buf("hello world   ");
        b.operate(Operator::Delete, Target::Motion(Motion::LastNonBlank), 1);
        assert_eq!(b.rope().to_string(), "   ");
    }

    #[test]
    fn percent_jumps_between_matching_brackets() {
        let mut b = buf("if (a[0]) { x }");
        b.apply_motion(Motion::MatchingBracket, false);
        assert_eq!(b.cursor.at, 8, "scanned forward to `(` and matched its `)`");

        // And back again from the closing side.
        b.apply_motion(Motion::MatchingBracket, false);
        assert_eq!(b.cursor.at, 3);

        // Nesting is counted, not just the next bracket of the same kind.
        let mut b = buf("((x))");
        b.apply_motion(Motion::MatchingBracket, false);
        assert_eq!(b.cursor.at, 4, "the outer pair");

        // Nothing to match is not an error; it simply does not move.
        let mut b = buf("plain text");
        b.apply_motion(Motion::MatchingBracket, false);
        assert_eq!(b.cursor.at, 0);
    }

    #[test]
    fn braces_move_by_paragraph() {
        let mut b = buf("one\ntwo\n\nthree\n\nfour");
        b.apply_motion(Motion::Paragraph { forward: true }, false);
        assert_eq!(b.row_of(b.cursor.at), 2, "the blank line after the first block");
        b.apply_motion(Motion::Paragraph { forward: true }, false);
        assert_eq!(b.row_of(b.cursor.at), 4);
        b.apply_motion(Motion::Paragraph { forward: false }, false);
        assert_eq!(b.row_of(b.cursor.at), 2, "and back");

        // Past the last paragraph it stops at the end rather than wrapping.
        b.apply_motion(Motion::Paragraph { forward: true }, false);
        b.apply_motion(Motion::Paragraph { forward: true }, false);
        assert_eq!(b.row_of(b.cursor.at), 5);
    }

    #[test]
    fn x_does_not_eat_the_newline() {
        let mut b = buf("ab\ncd");
        b.cursor = Cursor::at(1);
        b.operate(Operator::Delete, Target::Motion(Motion::Right), 1);
        assert_eq!(b.rope().to_string(), "a\ncd");
        // Deleting the last char of a line drags the cursor back onto a char.
        assert_eq!(b.cursor_col(), 0);

        // On an empty line there is nothing under the cursor to take.
        let mut b = buf("\ncd");
        b.operate(Operator::Delete, Target::Motion(Motion::Right), 1);
        assert_eq!(b.rope().to_string(), "\ncd");
    }

    #[test]
    fn dd_on_the_last_line_leaves_no_empty_line() {
        let mut b = buf("a\nb\nc");
        b.goto_row(2, false);
        b.operate(Operator::Delete, Target::Motion(Motion::CurrentLine), 1);
        assert_eq!(b.rope().to_string(), "a\nb");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.cursor_row(), 1);
    }

    #[test]
    fn dd_on_the_only_line_empties_the_buffer() {
        let mut b = buf("only");
        b.operate(Operator::Delete, Target::Motion(Motion::CurrentLine), 1);
        assert_eq!(b.rope().to_string(), "");
        assert_eq!(b.cursor.at, 0);
    }

    /// The cursor may never come to rest on the phantom row past the end of a
    /// file that ends in a newline: `dd` there has no line to take, and the
    /// range it works out comes back inside out.
    #[test]
    fn dd_over_and_over_on_the_last_blank_line_runs_out_of_lines_quietly() {
        let mut b = buf("a\n\n\n\n");
        b.goto_row(3, false);
        for _ in 0..6 {
            b.operate(Operator::Delete, Target::Motion(Motion::CurrentLine), 1);
            assert!(
                b.cursor_row() < b.line_count(),
                "{:?} left the cursor on row {} of {}",
                b.rope().to_string(),
                b.cursor_row(),
                b.line_count()
            );
        }
        assert_eq!(b.rope().to_string(), "");
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
        b.operate(Operator::Delete, Target::Motion(Motion::Right), 1);
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
        let path = std::env::temp_dir().join("bi_save_open_group.txt");
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

    fn op(text: &str, at: usize, op: Operator, target: Target, count: usize) -> String {
        let mut b = buf(text);
        b.cursor = Cursor::at(at);
        b.operate(op, target, count);
        b.rope().to_string()
    }

    #[test]
    fn dw_deletes_to_the_start_of_the_next_word() {
        assert_eq!(
            op(
                "foo bar baz",
                0,
                Operator::Delete,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                1
            ),
            "bar baz"
        );
    }

    #[test]
    fn dw_on_the_last_word_takes_all_of_it() {
        assert_eq!(
            op(
                "foo",
                0,
                Operator::Delete,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                1
            ),
            ""
        );
    }

    #[test]
    fn counted_dw_deletes_that_many_words() {
        assert_eq!(
            op(
                "a b c d",
                0,
                Operator::Delete,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                3
            ),
            "d"
        );
    }

    /// Vim quirk: `cw` on a non-blank behaves like `ce`, so the whitespace after
    /// the word survives. A literal `w` would have eaten it.
    #[test]
    fn cw_changes_the_word_and_leaves_the_spaces() {
        assert_eq!(
            op(
                "foo   bar",
                0,
                Operator::Change,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                1
            ),
            "   bar"
        );
    }

    #[test]
    fn cw_on_whitespace_is_not_special_and_acts_like_w() {
        assert_eq!(
            op(
                "a   bar",
                1,
                Operator::Change,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                1
            ),
            "abar"
        );
    }

    #[test]
    fn counted_cw_reaches_the_end_of_the_last_word() {
        assert_eq!(
            op(
                "foo   bar baz",
                0,
                Operator::Change,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                2
            ),
            " baz"
        );
    }

    /// Vim quirk: `dw` at the end of a line stops there instead of pulling the
    /// next line up.
    #[test]
    fn dw_at_the_end_of_a_line_does_not_join_lines() {
        assert_eq!(
            op(
                "hello\nworld",
                3,
                Operator::Delete,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                1
            ),
            "hel\nworld"
        );
    }

    #[test]
    fn d_dollar_deletes_through_the_last_char_but_not_the_newline() {
        assert_eq!(
            op("hello world\nnext", 6, Operator::Delete, Target::Motion(Motion::LineEnd), 1),
            "hello \nnext"
        );
    }

    #[test]
    fn d_zero_deletes_back_to_the_line_start() {
        assert_eq!(
            op("hello world", 6, Operator::Delete, Target::Motion(Motion::LineStart), 1),
            "world"
        );
    }

    #[test]
    fn db_deletes_the_previous_word() {
        assert_eq!(
            op(
                "foo bar",
                4,
                Operator::Delete,
                Target::Motion(Motion::Word { big: false, forward: false, end: false }),
                1
            ),
            "bar"
        );
    }

    #[test]
    fn dl_takes_the_char_under_the_cursor() {
        assert_eq!(op("abc", 1, Operator::Delete, Target::Motion(Motion::Right), 1), "ac");
    }

    #[test]
    fn dj_is_linewise_and_takes_both_lines() {
        assert_eq!(
            op("one\ntwo\nthree", 1, Operator::Delete, Target::Motion(Motion::Down), 1),
            "three"
        );
    }

    #[test]
    fn dk_is_linewise_upward() {
        assert_eq!(
            op("one\ntwo\nthree", 5, Operator::Delete, Target::Motion(Motion::Up), 1),
            "three"
        );
    }

    #[test]
    fn dgg_deletes_from_the_first_line_through_this_one() {
        assert_eq!(
            op("one\ntwo\nthree", 5, Operator::Delete, Target::Motion(Motion::FirstLine), 1),
            "three"
        );
    }

    #[test]
    fn dd_takes_the_whole_line_including_its_newline() {
        assert_eq!(
            op("one\ntwo\nthree", 1, Operator::Delete, Target::Motion(Motion::CurrentLine), 1),
            "two\nthree"
        );
    }

    #[test]
    fn counted_dd_takes_that_many_lines() {
        assert_eq!(
            op("one\ntwo\nthree", 1, Operator::Delete, Target::Motion(Motion::CurrentLine), 2),
            "three"
        );
    }

    #[test]
    fn dd_past_the_end_stops_at_the_last_line() {
        assert_eq!(
            op("one\ntwo", 0, Operator::Delete, Target::Motion(Motion::CurrentLine), 99),
            ""
        );
    }

    /// `cc` empties the line but keeps it, so insert mode has somewhere to go.
    #[test]
    fn cc_clears_the_line_without_removing_it() {
        assert_eq!(
            op("one\ntwo", 1, Operator::Change, Target::Motion(Motion::CurrentLine), 1),
            "\ntwo"
        );
    }

    #[test]
    fn an_operator_that_moves_nowhere_changes_nothing() {
        let mut b = buf("abc");
        b.cursor = Cursor::at(0);
        assert!(
            b.operate(
                Operator::Delete,
                Target::Motion(Motion::Word { big: false, forward: false, end: false }),
                1
            )
            .is_none(),
            "b at char 0 has no range"
        );
        assert_eq!(b.rope().to_string(), "abc");
    }

    // ---- capture -----------------------------------------------------------

    #[test]
    fn a_charwise_operator_captures_what_it_took() {
        let mut b = buf("foo bar");
        let e = b
            .operate(
                Operator::Delete,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                1,
            )
            .unwrap();
        assert_eq!(e.text, "foo ");
        assert_eq!(e.kind, Shape::Chars);
    }

    #[test]
    fn a_linewise_operator_captures_a_trailing_newline() {
        let mut b = buf("one\ntwo");
        let e = b.operate(Operator::Delete, Target::Motion(Motion::CurrentLine), 1).unwrap();
        assert_eq!(e.text, "one\n");
        assert_eq!(e.kind, Shape::Lines);
    }

    /// Even from a final line that has no newline of its own — a linewise entry
    /// is always a whole line, or pasting it could not open one.
    #[test]
    fn a_linewise_capture_from_the_last_line_still_ends_in_a_newline() {
        let mut b = buf("one\ntwo");
        b.cursor = Cursor::at(4);
        let e = b.operate(Operator::Yank, Target::Motion(Motion::CurrentLine), 1).unwrap();
        assert_eq!(e.text, "two\n");
    }

    #[test]
    fn yank_captures_without_touching_the_text() {
        let mut b = buf("foo bar");
        let e = b
            .operate(
                Operator::Yank,
                Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                1,
            )
            .unwrap();
        assert_eq!(e.text, "foo ");
        assert_eq!(b.rope().to_string(), "foo bar", "yank is not a mutation");
        assert!(b.pending_edits.is_empty(), "and logs no edit");
    }

    #[test]
    fn a_backward_yank_leaves_the_cursor_at_the_start_of_the_range() {
        let mut b = buf("foo bar");
        b.cursor = Cursor::at(4);
        let e = b
            .operate(
                Operator::Yank,
                Target::Motion(Motion::Word { big: false, forward: false, end: false }),
                1,
            )
            .unwrap();
        assert_eq!(e.text, "foo ");
        assert_eq!(b.cursor.at, 0);
    }

    // ---- paste -------------------------------------------------------------

    fn pasted(text: &str, at: usize, entry: Entry, before: bool, count: usize) -> String {
        let mut b = buf(text);
        b.cursor = Cursor::at(at);
        b.paste(&entry, before, count);
        b.rope().to_string()
    }

    fn chars(text: &str) -> Entry {
        Entry { text: text.into(), kind: Shape::Chars }
    }

    fn lines(text: &str) -> Entry {
        Entry { text: text.into(), kind: Shape::Lines }
    }

    #[test]
    fn charwise_p_lands_after_the_cursor_and_big_p_on_it() {
        assert_eq!(pasted("abc", 0, chars("XY"), false, 1), "aXYbc");
        assert_eq!(pasted("abc", 0, chars("XY"), true, 1), "XYabc");
    }

    #[test]
    fn charwise_paste_leaves_the_cursor_on_the_last_pasted_char() {
        let mut b = buf("abc");
        b.paste(&chars("XY"), false, 1);
        assert_eq!(b.rope().to_string(), "aXYbc");
        assert_eq!(b.cursor.at, 2);
    }

    /// There is no char to paste *after* on an empty line, so `p` stays put
    /// rather than stepping onto the next line.
    #[test]
    fn charwise_p_on_an_empty_line_does_not_cross_the_newline() {
        assert_eq!(pasted("\nabc", 0, chars("X"), false, 1), "X\nabc");
    }

    #[test]
    fn linewise_p_opens_a_line_below_and_big_p_above() {
        assert_eq!(pasted("one\ntwo", 1, lines("NEW\n"), false, 1), "one\nNEW\ntwo");
        assert_eq!(pasted("one\ntwo", 1, lines("NEW\n"), true, 1), "NEW\none\ntwo");
    }

    #[test]
    fn linewise_paste_below_the_final_line_adds_the_newline_it_needs() {
        assert_eq!(pasted("one\ntwo", 4, lines("NEW\n"), false, 1), "one\ntwo\nNEW");
    }

    #[test]
    fn linewise_paste_leaves_the_cursor_on_the_first_pasted_line() {
        let mut b = buf("one\ntwo");
        b.paste(&lines("NEW\n"), false, 1);
        assert_eq!(b.rope().to_string(), "one\nNEW\ntwo");
        assert_eq!(b.cursor_row(), 1);
        assert_eq!(b.cursor_col(), 0);
    }

    #[test]
    fn a_counted_paste_repeats_the_content() {
        assert_eq!(pasted("abc", 0, chars("X"), false, 3), "aXXXbc");
        assert_eq!(pasted("one", 0, lines("NEW\n"), true, 2), "NEW\nNEW\none");
    }

    /// Repeating a linewise entry must not collapse the blank line inside it.
    #[test]
    fn a_repeated_linewise_paste_keeps_its_blank_lines() {
        assert_eq!(pasted("one", 0, lines("a\n\n"), false, 1), "one\na\n");
    }

    #[test]
    fn a_paste_is_a_single_undo_step() {
        let mut b = buf("abc");
        b.paste(&chars("XY"), false, 3);
        b.commit_undo();
        assert_eq!(b.rope().to_string(), "aXYXYXYbc");

        b.undo();
        assert_eq!(b.rope().to_string(), "abc");
    }

    #[test]
    fn delete_leaves_the_cursor_on_a_char() {
        let mut b = buf("foo bar");
        b.cursor = Cursor::at(4);
        b.operate(Operator::Delete, Target::Motion(Motion::LineEnd), 1);
        assert_eq!(b.rope().to_string(), "foo ");
        assert_eq!(b.cursor.at, 3, "pulled back onto the last remaining char");
    }

    #[test]
    fn change_leaves_the_cursor_where_the_text_was() {
        let mut b = buf("foo bar");
        b.cursor = Cursor::at(4);
        b.operate(Operator::Change, Target::Motion(Motion::LineEnd), 1);
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

    // ---- r / ~ / J ---------------------------------------------------------

    #[test]
    fn r_overwrites_in_place_and_rests_on_the_last_char() {
        let mut b = buf("abcdef");
        b.cursor = Cursor::at(1);
        assert!(b.replace_chars('x', 3));
        assert_eq!(b.rope().to_string(), "axxxef");
        assert_eq!(b.cursor_col(), 3, "vim leaves the cursor on the last replaced char");
    }

    #[test]
    fn r_refuses_rather_than_partially_replacing_a_short_tail() {
        let mut b = buf("abc\ndef");
        b.cursor = Cursor::at(1);
        assert!(!b.replace_chars('x', 5));
        assert_eq!(b.rope().to_string(), "abc\ndef", "nothing changed");
    }

    #[test]
    fn r_never_reaches_across_a_newline() {
        let mut b = buf("ab\ncd");
        b.cursor = Cursor::at(0);
        assert!(!b.replace_chars('x', 3));
        assert_eq!(b.rope().to_string(), "ab\ncd");
    }

    #[test]
    fn tilde_flips_case_and_walks_right() {
        let mut b = buf("hello world");
        b.toggle_case(5);
        assert_eq!(b.rope().to_string(), "HELLO world");
        assert_eq!(b.cursor_col(), 5);
    }

    #[test]
    fn tilde_steps_over_caseless_chars_and_stops_at_the_line_end() {
        let mut b = buf("a1b\nxy");
        b.toggle_case(99);
        assert_eq!(b.rope().to_string(), "A1B\nxy", "did not wrap onto the next line");
    }

    #[test]
    fn j_joins_with_one_space_and_eats_the_indent() {
        let mut b = buf("foo\n    bar\nbaz");
        b.join_lines(1);
        assert_eq!(b.rope().to_string(), "foo bar\nbaz");
        assert_eq!(b.cursor_col(), 3, "cursor lands on the join");
    }

    #[test]
    fn j_adds_no_space_after_trailing_whitespace_or_before_a_blank_line() {
        let mut b = buf("foo \nbar");
        b.join_lines(1);
        assert_eq!(b.rope().to_string(), "foo bar", "no double space");

        let mut b = buf("foo\n\nbar");
        b.join_lines(1);
        assert_eq!(b.rope().to_string(), "foo\nbar", "nothing to separate");
    }

    #[test]
    fn a_count_joins_that_many_lines_and_1j_means_the_same_as_2j() {
        let mut b = buf("a\nb\nc\nd");
        b.join_lines(3);
        assert_eq!(b.rope().to_string(), "a b c\nd");

        let mut b = buf("a\nb\nc");
        b.join_lines(1);
        let mut c = buf("a\nb\nc");
        c.join_lines(2);
        assert_eq!(b.rope().to_string(), c.rope().to_string());
    }

    #[test]
    fn j_on_the_last_line_does_nothing() {
        let mut b = buf("only");
        b.join_lines(9);
        assert_eq!(b.rope().to_string(), "only");
    }

    #[test]
    fn the_new_commands_are_each_one_undo_step() {
        let mut b = buf("foo\nbar");
        b.join_lines(1);
        b.commit_undo();
        assert_eq!(b.rope().to_string(), "foo bar");
        assert!(b.undo());
        assert_eq!(b.rope().to_string(), "foo\nbar");
    }

    // ---- find-char ---------------------------------------------------------

    fn find(text: &str, at: usize, m: Motion) -> Option<usize> {
        let mut b = buf(text);
        b.cursor = Cursor::at(at);
        let before = b.cursor.at;
        b.apply_motion(m, false);
        (b.cursor.at != before || before == 0).then_some(b.cursor.at)
    }

    fn f(ch: char, forward: bool, till: bool) -> Motion {
        Motion::FindChar { ch, forward, till, repeat: false }
    }

    #[test]
    fn f_lands_on_the_char_and_t_stops_before_it() {
        //          0123456789
        let text = "foo bar baz";
        assert_eq!(find(text, 0, f('b', true, false)), Some(4), "f b");
        assert_eq!(find(text, 0, f('b', true, true)), Some(3), "t b stops one short");
    }

    #[test]
    fn big_f_searches_backwards_and_big_t_stops_after() {
        let text = "foo bar baz";
        assert_eq!(find(text, 10, f('b', false, false)), Some(8), "F b");
        assert_eq!(find(text, 10, f('b', false, true)), Some(9), "T b stops one after");
    }

    #[test]
    fn a_find_never_leaves_the_line() {
        let mut b = buf("abc\nxbz");
        b.cursor = Cursor::at(0);
        b.apply_motion(f('z', true, false), false);
        assert_eq!(b.cursor.at, 0, "the z is on the next line, so nothing moved");

        b.cursor = Cursor::at(5);
        b.apply_motion(f('a', false, false), false);
        assert_eq!(b.cursor.at, 5, "the a is on the previous line");
    }

    #[test]
    fn a_counted_find_reaches_the_nth_occurrence() {
        let mut b = buf("a,b,c,d");
        b.cursor = Cursor::at(0);
        assert_eq!(b.find_char(b.cursor, ',', true, false, false, 3).map(|c| c.at), Some(5));
        // `t` with a count stops before the nth, not before the first.
        assert_eq!(b.find_char(b.cursor, ',', true, true, false, 3).map(|c| c.at), Some(4));
    }

    #[test]
    fn f_finds_the_next_occurrence_not_the_one_under_the_cursor() {
        let mut b = buf("xaxa");
        b.cursor = Cursor::at(0);
        b.apply_motion(f('x', true, false), false);
        assert_eq!(b.cursor.at, 2, "started on an x, so it moved to the next one");
    }

    #[test]
    fn df_is_inclusive_and_d_big_f_is_exclusive() {
        // `df)` takes the bracket; `dF(` leaves it.
        assert_eq!(op("a(bc)d", 1, Operator::Delete, Target::Motion(f(')', true, false)), 1), "ad",);
        assert_eq!(
            op("a(bc)d", 4, Operator::Delete, Target::Motion(f('(', false, false)), 1),
            "a)d",
        );
    }

    #[test]
    fn an_operator_over_a_find_that_misses_changes_nothing() {
        let mut b = buf("abc");
        b.cursor = Cursor::at(0);
        assert!(b.operate(Operator::Delete, Target::Motion(f('z', true, false)), 1).is_none());
        assert_eq!(b.rope().to_string(), "abc");
    }

    fn obj(text: &str, at: usize, object: TextObject, around: bool) -> Option<String> {
        let mut b = buf(text);
        b.cursor = Cursor::at(at);
        let (s, e) = b.object_range(object, around)?;
        Some(b.rope().slice(s..e).to_string())
    }

    const WORD: TextObject = TextObject::Word { big: false };
    const BIG_WORD: TextObject = TextObject::Word { big: true };

    #[test]
    fn iw_is_the_word_under_the_cursor_from_anywhere_in_it() {
        for at in 4..7 {
            assert_eq!(obj("foo bar baz", at, WORD, false).as_deref(), Some("bar"), "at {at}");
        }
    }

    #[test]
    fn iw_on_whitespace_is_the_run_of_whitespace() {
        assert_eq!(obj("a   b", 2, WORD, false).as_deref(), Some("   "));
    }

    #[test]
    fn iw_stops_at_punctuation_but_a_big_word_does_not() {
        assert_eq!(obj("foo.bar", 0, WORD, false).as_deref(), Some("foo"));
        assert_eq!(obj("foo.bar", 0, BIG_WORD, false).as_deref(), Some("foo.bar"));
    }

    #[test]
    fn aw_takes_the_whitespace_after_the_word() {
        assert_eq!(obj("foo bar baz", 0, WORD, true).as_deref(), Some("foo "));
    }

    /// Vim's rule: with nothing trailing to take, `aw` reaches backwards
    /// instead, so `daw` on the last word does not leave a dangling space.
    #[test]
    fn aw_at_the_end_of_a_line_takes_the_whitespace_before_it() {
        assert_eq!(obj("foo bar", 4, WORD, true).as_deref(), Some(" bar"));
    }

    #[test]
    fn a_word_object_never_crosses_a_line() {
        assert_eq!(obj("foo\nbar", 1, WORD, false).as_deref(), Some("foo"));
        assert_eq!(obj("foo\nbar", 1, WORD, true).as_deref(), Some("foo"));
    }

    #[test]
    fn i_quote_is_the_contents_and_a_quote_includes_them() {
        let text = "say \"hello there\" now";
        assert_eq!(obj(text, 8, TextObject::Quoted('"'), false).as_deref(), Some("hello there"));
        // `a"` takes the trailing space too, exactly as `aw` does — verified
        // against vim.
        assert_eq!(
            obj(text, 8, TextObject::Quoted('"'), true).as_deref(),
            Some("\"hello there\" ")
        );
    }

    #[test]
    fn a_quote_object_works_from_either_quote_itself() {
        let text = "\"abc\"";
        assert_eq!(obj(text, 0, TextObject::Quoted('"'), false).as_deref(), Some("abc"));
        assert_eq!(obj(text, 4, TextObject::Quoted('"'), false).as_deref(), Some("abc"));
    }

    #[test]
    fn quotes_pair_in_order_along_the_line() {
        // Cursor between the pairs is inside neither.
        let text = "\"a\" x \"b\"";
        assert_eq!(obj(text, 1, TextObject::Quoted('"'), false).as_deref(), Some("a"));
        assert_eq!(obj(text, 7, TextObject::Quoted('"'), false).as_deref(), Some("b"));
        assert_eq!(obj(text, 4, TextObject::Quoted('"'), false), None, "in the gap");
    }

    #[test]
    fn an_empty_quote_pair_has_nothing_inside_but_can_still_be_taken_around() {
        assert_eq!(obj("x\"\"y", 1, TextObject::Quoted('"'), false), None);
        assert_eq!(obj("x\"\"y", 1, TextObject::Quoted('"'), true).as_deref(), Some("\"\""));
    }

    #[test]
    fn i_paren_is_the_contents_and_a_paren_includes_the_brackets() {
        let text = "f(a, b)";
        assert_eq!(obj(text, 3, TextObject::Delimited('('), false).as_deref(), Some("a, b"));
        assert_eq!(obj(text, 3, TextObject::Delimited('('), true).as_deref(), Some("(a, b)"));
    }

    /// The reason nesting has to be counted: the naive scan finds the wrong
    /// pair the moment brackets are nested.
    #[test]
    fn a_delimited_object_counts_nesting_on_the_way_out() {
        //          0123456789
        let text = "f(g(x), y)";
        assert_eq!(obj(text, 4, TextObject::Delimited('('), false).as_deref(), Some("x"));
        assert_eq!(obj(text, 7, TextObject::Delimited('('), false).as_deref(), Some("g(x), y"));
    }

    #[test]
    fn a_delimited_object_counts_nesting_on_the_way_in_too() {
        assert_eq!(
            obj("((a))", 0, TextObject::Delimited('('), false).as_deref(),
            Some("(a)"),
            "from the outer bracket, the inner pair is the contents",
        );
    }

    #[test]
    fn a_delimited_object_spans_lines() {
        let text = "fn a() {\n    body\n}";
        // Whole-line contents make `i{` linewise, so this is the body *line*
        // rather than the exact span between the braces. vim does the same,
        // which is why `di{` leaves `{` and `}` on their own lines.
        assert_eq!(obj(text, 13, TextObject::Delimited('{'), false).as_deref(), Some("    body\n"),);
    }

    #[test]
    fn a_delimited_object_outside_any_pair_is_none() {
        assert_eq!(obj("no brackets", 3, TextObject::Delimited('('), false), None);
        assert_eq!(obj("a(b) c", 5, TextObject::Delimited('('), false), None);
    }

    #[test]
    fn ip_is_the_run_of_non_blank_lines() {
        let text = "one\ntwo\n\nthree\n";
        assert_eq!(obj(text, 0, TextObject::Paragraph, false).as_deref(), Some("one\ntwo"));
        assert_eq!(obj(text, 9, TextObject::Paragraph, false).as_deref(), Some("three"));
    }

    #[test]
    fn ap_reaches_into_the_blank_lines_after_it() {
        let text = "one\n\n\ntwo\n";
        assert_eq!(obj(text, 0, TextObject::Paragraph, true).as_deref(), Some("one\n\n"));
    }

    // ---- operators over objects --------------------------------------------

    #[test]
    fn diw_and_daw_differ_by_the_trailing_space() {
        let iw = Target::Object { object: WORD, around: false };
        let aw = Target::Object { object: WORD, around: true };
        assert_eq!(op("foo bar baz", 4, Operator::Delete, iw, 1), "foo  baz");
        assert_eq!(op("foo bar baz", 4, Operator::Delete, aw, 1), "foo baz");
    }

    #[test]
    fn ci_quote_empties_the_string_and_leaves_the_quotes() {
        let target = Target::Object { object: TextObject::Quoted('"'), around: false };
        assert_eq!(op("say \"hello\"", 6, Operator::Change, target, 1), "say \"\"");
    }

    #[test]
    fn di_paren_leaves_the_brackets_behind() {
        let target = Target::Object { object: TextObject::Delimited('('), around: false };
        assert_eq!(op("f(a, b)", 3, Operator::Delete, target, 1), "f()");
    }

    #[test]
    fn yanking_an_object_captures_it_without_changing_the_text() {
        let mut b = buf("foo bar");
        b.cursor = Cursor::at(4);
        let target = Target::Object { object: WORD, around: false };
        let e = b.operate(Operator::Yank, target, 1).unwrap();
        assert_eq!(e.text, "bar");
        assert_eq!(e.kind, Shape::Chars);
        assert_eq!(b.rope().to_string(), "foo bar");
    }

    /// A paragraph is linewise, so `dip` must take the line terminator with it
    /// rather than leaving an empty line, and the capture must end in a newline
    /// so pasting it back opens a line.
    #[test]
    fn dip_is_linewise() {
        let mut b = buf("one\ntwo\n\nthree\n");
        b.cursor = Cursor::at(0);
        let target = Target::Object { object: TextObject::Paragraph, around: false };
        let e = b.operate(Operator::Delete, target, 1).unwrap();
        assert_eq!(e.kind, Shape::Lines);
        assert_eq!(e.text, "one\ntwo\n");
        assert_eq!(b.rope().to_string(), "\nthree\n", "no empty line left over");
    }

    /// `cip` keeps the line for insert mode to sit on, exactly as `cc` does.
    #[test]
    fn cip_keeps_a_line_to_type_on() {
        let mut b = buf("one\ntwo\n\nthree\n");
        b.cursor = Cursor::at(0);
        let target = Target::Object { object: TextObject::Paragraph, around: false };
        b.operate(Operator::Change, target, 1);
        assert_eq!(b.rope().to_string(), "\n\nthree\n");
    }

    #[test]
    fn an_object_the_cursor_is_not_inside_changes_nothing() {
        let mut b = buf("abc");
        b.cursor = Cursor::at(1);
        let target = Target::Object { object: TextObject::Delimited('('), around: false };
        assert!(b.operate(Operator::Delete, target, 1).is_none());
        assert_eq!(b.rope().to_string(), "abc");
    }

    // ---- conformance fixes found by differential testing against vim -------

    /// `;` after `t` has to skip the match it is already parked next to, or it
    /// never advances. A freshly typed `t` must not skip. Found by running
    /// `t.;x` through both editors.
    #[test]
    fn a_repeated_till_skips_the_match_it_is_already_next_to() {
        let fresh = Motion::FindChar { ch: '.', forward: true, till: true, repeat: false };
        let again = Motion::FindChar { ch: '.', forward: true, till: true, repeat: true };

        let mut b = buf("a.b.c.d");
        b.apply_motion(fresh, false);
        assert_eq!(b.cursor.at, 0, "a fresh t. from column 0 stays put, as in vim");

        b.apply_motion(again, false);
        assert_eq!(b.cursor.at, 2, "but ; moves on to before the next dot");
    }

    #[test]
    fn a_repeated_till_backwards_skips_too() {
        let again = Motion::FindChar { ch: '.', forward: false, till: true, repeat: true };
        let mut b = buf("a.b.c.d");
        b.cursor = Cursor::at(4);
        b.apply_motion(again, false);
        assert_eq!(b.cursor.at, 2);
    }

    /// `dG` through the last line of a file that already ends in a newline must
    /// leave that newline alone. The "take the preceding newline" rule only
    /// applies when the buffer has no terminator of its own.
    #[test]
    fn deleting_to_the_end_keeps_a_trailing_newline_that_was_already_there() {
        let mut b = buf("one\ntwo\nthree\n");
        b.goto_row(1, false);
        b.operate(Operator::Delete, Target::Motion(Motion::LastLine), 1);
        assert_eq!(b.rope().to_string(), "one\n");
    }

    #[test]
    fn deleting_to_the_end_of_a_file_without_one_does_not_invent_a_newline() {
        let mut b = buf("one\ntwo\nthree");
        b.goto_row(1, false);
        b.operate(Operator::Delete, Target::Motion(Motion::LastLine), 1);
        assert_eq!(b.rope().to_string(), "one");
    }

    /// `a"` takes the whitespace after the closing quote, so `da"` leaves one
    /// space rather than two.
    #[test]
    fn a_quote_takes_the_space_after_it() {
        let target = Target::Object { object: TextObject::Quoted('"'), around: false };
        let around = Target::Object { object: TextObject::Quoted('"'), around: true };
        assert_eq!(op("say \"hi\" ok", 5, Operator::Delete, target, 1), "say \"\" ok");
        assert_eq!(op("say \"hi\" ok", 5, Operator::Delete, around, 1), "say ok");
    }

    #[test]
    fn a_quote_falls_back_to_the_space_before_it() {
        let around = Target::Object { object: TextObject::Quoted('"'), around: true };
        assert_eq!(op("say \"hi\"", 5, Operator::Delete, around, 1), "say");
    }

    /// An inner block whose contents are whole lines is linewise, so `di{`
    /// leaves the braces on their own lines instead of collapsing them.
    #[test]
    fn an_inner_block_of_whole_lines_is_linewise() {
        let target = Target::Object { object: TextObject::Delimited('{'), around: false };
        assert_eq!(op("fn a() {\n    body\n}\n", 13, Operator::Delete, target, 1), "fn a() {\n}\n",);
    }

    /// But only when the brackets have their lines to themselves — a partial
    /// line stays charwise.
    #[test]
    fn an_inner_block_sharing_a_line_stays_charwise() {
        let target = Target::Object { object: TextObject::Delimited('('), around: false };
        assert_eq!(op("f(a,\n b)", 2, Operator::Delete, target, 1), "f()");
    }
}
