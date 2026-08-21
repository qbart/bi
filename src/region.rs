//! What an operation applies to.
//!
//! Every editing command has to answer the same question before it can do
//! anything: *which characters?* The answer used to be re-derived at each
//! point of use, from `Selections`, the mode, and an optional line range —
//! nine derivations, no two agreeing. This is that answer, computed once at
//! the boundary and passed down.
//!
//! See `docs/specs/regions.md`.

use crate::buffer::{Buffer, Cursor, Edit};
use crate::selection::{Selection, Selections};

/// How a region was meant.
///
/// One enum for three questions that were three enums: what kind of selection
/// is on screen, how text in a register was taken, and what `.` repeats. They
/// were always the same three answers, and keeping them apart meant keeping
/// them in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A span within a line, or several lines' worth of one. Pastes inline.
    Chars,
    /// Whole lines. A register entry of this shape always ends in a newline,
    /// even when it came from a final line that had none, or pasting it back
    /// could not open a line.
    Lines,
    /// A rectangle. A register entry of this shape joins its rows with `\n`
    /// and has no trailing one — the newlines separate the rows of the block
    /// rather than terminating lines of the buffer.
    Block,
}

/// One contiguous stretch of the buffer.
///
/// A charwise part runs straight through line terminators, because that is
/// what selecting across two lines and pressing `d` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub start: usize,
    pub end: usize,
}

/// One row's worth of a region, never crossing a line terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

impl Part {
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// The same part, out to the line terminator after it.
    ///
    /// What a linewise cut needs and a linewise change does not: `dd` takes
    /// the line away, `cc` keeps it for insert mode to sit on.
    pub fn terminated(self, buffer: &Buffer) -> Self {
        let (start, end) = buffer.line_range(self.start, self.end, true);
        Self { start, end }
    }
}

impl Span {
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

/// The region an operation applies to.
///
/// Two views of one answer, because operations genuinely ask two questions:
///
/// - [`Region::parts`] is contiguous stretches, one per selection — what an
///   operator that takes text away works on. `d` over two selected lines takes
///   one range with the newline in the middle of it.
/// - [`Region::spans`] is those parts cut at every row boundary — what a
///   rewrite that must not touch a line terminator works on. `r`, `:case` and
///   `:s` are all this.
///
/// A rectangle's parts are already one per row, so for a block the two views
/// are the same list. That is the whole of what "blockwise" means here.
///
/// Not `Selections`, which sorts *and merges*: two rows whose spans meet at a
/// line end would silently weld together. A region has no merge invariant,
/// because it is not a set of cursors — it is the answer to one question,
/// thrown away afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    shape: Shape,
    parts: Vec<Part>,
}

impl Region {
    /// From what is on screen: every selection, shaped by `shape`.
    ///
    /// Multi-cursor needs no arm of its own. Three collapsed selections are
    /// three one-character parts, and every operation over them is the same
    /// loop as everything else.
    ///
    /// `to_eol` is blockwise `$` — the right edge is each row's line end.
    pub fn of(buffer: &Buffer, selections: &Selections, shape: Shape, to_eol: bool) -> Self {
        let parts = match shape {
            Shape::Block => block_parts(buffer, selections, to_eol),
            _ => selections.all().iter().map(|&s| part_of(buffer, s, shape)).collect(),
        };
        Self { shape, parts }
    }

    /// What one selection contributes to a region of this shape.
    ///
    /// Public because an operator that walks its selections one at a time —
    /// capturing a register entry and landing a cursor for each — still has to
    /// ask what the shape means, and asking it here is what keeps `line_range`
    /// from being spelled out again at every such loop.
    pub fn part_of(buffer: &Buffer, selection: Selection, shape: Shape) -> Part {
        part_of(buffer, selection, shape)
    }

    /// Whole rows `first..=last`, which is what a `:` line's addresses name.
    ///
    /// One part per row rather than one covering them all, because `:s`
    /// without `g` substitutes once *per line* and a region that had forgotten
    /// where the lines were could not say that.
    pub fn of_rows(buffer: &Buffer, first: usize, last: usize) -> Self {
        let rows = buffer.line_count();
        let last = last.min(rows.saturating_sub(1));
        let parts = (first..=last.max(first))
            .filter(|&row| row < rows)
            .map(|row| {
                let start = buffer.rope().line_to_char(row);
                Part { start, end: start + buffer.line_len(row) }
            })
            .collect();
        Self { shape: Shape::Lines, parts }
    }

    /// From explicit char ranges — what a text object gives.
    pub fn spanning(shape: Shape, ranges: impl IntoIterator<Item = (usize, usize)>) -> Self {
        let mut parts: Vec<Part> =
            ranges.into_iter().map(|(start, end)| Part { start, end: end.max(start) }).collect();
        parts.sort_by_key(|part| (part.start, part.end));
        Self { shape, parts }
    }

    /// A region covering nothing. What a command with nothing to act on gets.
    pub fn empty(shape: Shape) -> Self {
        Self { shape, parts: Vec::new() }
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// Contiguous stretches, one per selection — or one per row for a
    /// rectangle. What an operator that takes text away works on.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The parts cut at every row boundary and clipped to each row's content.
    ///
    /// What a rewrite that must never touch a line terminator works on. A
    /// row a rectangle overhangs comes back empty and *stays in the list*: a
    /// block is a rectangle even where the text is not, and dropping the short
    /// rows would lose the shape a yanked block has to keep.
    pub fn spans(&self, buffer: &Buffer) -> Vec<Span> {
        self.parts
            .iter()
            .flat_map(|part| split_rows(buffer, part.start, part.end, self.shape == Shape::Block))
            .collect()
    }

    /// The same region with each part carrying its line terminator.
    pub fn terminated(&self, buffer: &Buffer) -> Self {
        let parts = self.parts.iter().map(|part| part.terminated(buffer)).collect();
        Self { shape: self.shape, parts }
    }

    /// Whether there is any text here. A rectangle over three empty rows has
    /// parts and no text, and every operation over it is a no-op.
    pub fn is_empty(&self) -> bool {
        self.parts.iter().all(|part| part.is_empty())
    }

    /// The first character the region covers, or the top-left corner of a
    /// rectangle. Where the cursor lands after most rewrites.
    pub fn start(&self) -> usize {
        self.parts.first().map_or(0, |part| part.start)
    }

    /// The rows the region touches, first and last.
    pub fn row_range(&self, buffer: &Buffer) -> Option<(usize, usize)> {
        let first = self.parts.first()?;
        let last = self.parts.last()?;
        Some((
            buffer.row_at(Cursor::at(first.start)),
            buffer.row_at(Cursor::at(last.end.max(last.start))),
        ))
    }

    /// How many rows carry text.
    pub fn filled_rows(&self, buffer: &Buffer) -> usize {
        self.spans(buffer).iter().filter(|span| !span.is_empty()).count()
    }

    /// The text of each part, in order.
    pub fn texts(&self, buffer: &Buffer) -> Vec<String> {
        self.parts.iter().map(|part| buffer.slice(part.start, part.end)).collect()
    }

    /// The whole region as one string, ready for a register.
    ///
    /// The one place a shape decides how text is spelled, so a register entry
    /// and the region it came from cannot disagree: a rectangle joins its rows
    /// with `\n` and stops, and lines always end in one.
    pub fn text(&self, buffer: &Buffer) -> String {
        let mut text = self.texts(buffer).join("\n");
        if self.shape == Shape::Lines && !text.ends_with('\n') {
            text.push('\n');
        }
        text
    }

    /// Rewrites every part through `f`, and hands back the edits it made.
    ///
    /// Last part first: an edit shifts everything below it and nothing above,
    /// so descending order keeps every part's position valid without a
    /// correction pass. Written once here rather than at the four call sites
    /// that each had their own copy of the comment.
    ///
    /// The edits are the buffer's own, so carrying a position across them is
    /// [`Region::carry`] — the same [`Edit::map`] fold `trim` and `retab`
    /// already use, rather than a third strategy.
    pub fn rewrite(&self, buffer: &mut Buffer, f: impl Fn(&str) -> String) -> Vec<Edit> {
        self.rewrite_parts(buffer, &self.parts, f)
    }

    /// The same, a row at a time — for a rewrite that must not reach across a
    /// line terminator.
    pub fn rewrite_rows(&self, buffer: &mut Buffer, f: impl Fn(&str) -> String) -> Vec<Edit> {
        let parts: Vec<Part> =
            self.spans(buffer).iter().map(|s| Part { start: s.start, end: s.end }).collect();
        self.rewrite_parts(buffer, &parts, f)
    }

    fn rewrite_parts(
        &self,
        buffer: &mut Buffer,
        parts: &[Part],
        f: impl Fn(&str) -> String,
    ) -> Vec<Edit> {
        let base = buffer.pending_edits.len();
        for part in parts.iter().rev() {
            if part.is_empty() {
                continue;
            }
            let text = f(&buffer.slice(part.start, part.end));
            buffer.replace_range(part.start, part.end, &text);
        }
        buffer.pending_edits[base..].to_vec()
    }

    /// Applies an operator over every part, bottom to top.
    ///
    /// Same ordering and same reason as [`Region::rewrite`]: a cut shifts
    /// everything below it and nothing above.
    pub fn cut(&self, buffer: &mut Buffer, op: crate::motion::Operator) {
        for part in self.parts.iter().rev() {
            if part.is_empty() {
                continue;
            }
            buffer.operate_range(Cursor::at(part.start), op, part.start, part.end, false);
        }
    }

    /// Carries a position across what a rewrite did to the buffer.
    pub fn carry(edits: &[Edit], at: usize) -> usize {
        edits.iter().fold(at, |at, edit| edit.map(at))
    }
}

/// What one selection covers under a given shape.
fn part_of(buffer: &Buffer, selection: Selection, shape: Shape) -> Part {
    let (start, end) = match shape {
        Shape::Lines => {
            let (lo, hi) = selection.range();
            buffer.line_range(lo, hi, false)
        }
        // Charwise visual includes the character under the head.
        _ => selection.inclusive_range(buffer.rope().len_chars()),
    };
    Part { start, end }
}

/// One span per row between two char offsets, clipped to each row's content.
///
/// `keep_empty` is the rectangle's rule: a row too short to reach the left
/// edge is still a row of the block.
fn split_rows(buffer: &Buffer, lo: usize, hi: usize, keep_empty: bool) -> Vec<Span> {
    let hi = hi.max(lo);
    let first = buffer.row_at(Cursor::at(lo));
    let last = buffer.row_at(Cursor::at(hi));
    (first..=last)
        .map(|row| {
            let start = buffer.rope().line_to_char(row);
            let end = start + buffer.line_len(row);
            let (from, to) = (start.max(lo), end.min(hi));
            Span { row, start: from, end: to.max(from) }
        })
        .filter(|span| keep_empty || !span.is_empty())
        .collect()
}

/// One part per row the rectangle covers, top to bottom.
fn block_parts(buffer: &Buffer, selections: &Selections, to_eol: bool) -> Vec<Part> {
    let (lo, hi) = selections.primary().range();
    let (first, last) = (buffer.row_at(Cursor::at(lo)), buffer.row_at(Cursor::at(hi)));
    (first..=last)
        .map(|row| {
            let span = block_span_at(buffer, selections, to_eol, row);
            Part { start: span.start, end: span.end }
        })
        .collect()
}

/// The rectangle's columns.
pub fn block_columns(buffer: &Buffer, selections: &Selections) -> (usize, usize) {
    let selection = selections.primary();
    let a = buffer.col_at(selection.anchor);
    let b = buffer.col_at(selection.head);
    (a.min(b), a.max(b))
}

/// The rectangle's span on one row.
///
/// What the renderer asks, a row at a time — building the whole list per
/// visible row would put the block's height into the cost of a frame, which is
/// the one thing rendering here avoids.
pub fn block_span_at(buffer: &Buffer, selections: &Selections, to_eol: bool, row: usize) -> Span {
    let (left, right) = block_columns(buffer, selections);
    let start = buffer.rope().line_to_char(row);
    let len = buffer.line_len(row);
    let from = left.min(len);
    let to = if to_eol { len } else { (right + 1).min(len) };
    Span { row, start: start + from, end: start + to.max(from) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> Buffer {
        let mut buffer = Buffer::empty();
        buffer.insert_str(Cursor::at(0), text);
        buffer
    }

    fn sels(pairs: &[(usize, usize)]) -> Selections {
        let mut selections = Selections::default();
        selections.set(
            pairs
                .iter()
                .map(|&(a, h)| Selection { anchor: Cursor::at(a), head: Cursor::at(h) })
                .collect(),
        );
        selections
    }

    fn rows(region: &Region, buffer: &Buffer) -> Vec<String> {
        region.spans(buffer).iter().map(|s| buffer.slice(s.start, s.end)).collect()
    }

    #[test]
    fn a_charwise_region_is_the_selected_characters() {
        let buffer = buffer("hello world\n");
        let region = Region::of(&buffer, &sels(&[(0, 4)]), Shape::Chars, false);
        assert_eq!(region.texts(&buffer), ["hello"], "the head's char is included");
    }

    /// The two views, and why there are two: `d` takes one range with the
    /// newline inside it, while `r` and `:case` see a row at a time.
    #[test]
    fn a_charwise_part_runs_through_a_line_end_and_its_spans_do_not() {
        let buffer = buffer("one\ntwo\n");
        let region = Region::of(&buffer, &sels(&[(1, 5)]), Shape::Chars, false);
        assert_eq!(region.texts(&buffer), ["ne\ntw"]);
        assert_eq!(rows(&region, &buffer), ["ne", "tw"]);
    }

    #[test]
    fn a_linewise_region_is_whole_rows_without_the_terminator() {
        let buffer = buffer("one\ntwo\nthree\n");
        let region = Region::of(&buffer, &sels(&[(1, 5)]), Shape::Lines, false);
        assert_eq!(region.texts(&buffer), ["one\ntwo"]);
        assert_eq!(rows(&region, &buffer), ["one", "two"]);
        assert_eq!(region.terminated(&buffer).texts(&buffer), ["one\ntwo\n"], "what `dd` takes");
    }

    #[test]
    fn a_blockwise_region_is_the_columns_and_nothing_else() {
        let buffer = buffer("let ALPHA = 1;\nlet BETA  = 2;\nlet GAMMA = 3;\n");
        // Columns 4..=8 of three rows.
        let region = Region::of(&buffer, &sels(&[(4, 15 + 15 + 8)]), Shape::Block, false);
        assert_eq!(region.texts(&buffer), ["ALPHA", "BETA ", "GAMMA"]);
        assert_eq!(rows(&region, &buffer), region.texts(&buffer), "one row each either way");
    }

    #[test]
    fn a_row_too_short_for_the_rectangle_keeps_its_empty_span() {
        let buffer = buffer("aaaa\nbb\ncccc\n");
        // Columns 2..=3 of three rows; the middle row stops at 2.
        let region = Region::of(&buffer, &sels(&[(2, 8 + 3)]), Shape::Block, false);
        assert_eq!(rows(&region, &buffer), ["aa", "", "cc"], "a rectangle keeps its shape");
        assert_eq!(region.spans(&buffer).len(), 3);
        assert_eq!(region.filled_rows(&buffer), 2);
    }

    #[test]
    fn ragged_right_takes_every_row_to_its_own_end() {
        let buffer = buffer("aaaa\nbb\ncccccc\n");
        let region = Region::of(&buffer, &sels(&[(1, 8 + 1)]), Shape::Block, true);
        assert_eq!(region.texts(&buffer), ["aaa", "b", "ccccc"]);
    }

    /// Multi-cursor needs no arm: three cursors are three parts.
    #[test]
    fn every_selection_contributes_its_own_part() {
        let buffer = buffer("one two six\n");
        let region = Region::of(&buffer, &sels(&[(0, 2), (4, 6), (8, 10)]), Shape::Chars, false);
        assert_eq!(region.texts(&buffer), ["one", "two", "six"]);
    }

    #[test]
    fn a_row_region_is_the_lines_it_names() {
        let buffer = buffer("one\ntwo\nthree\n");
        assert_eq!(Region::of_rows(&buffer, 1, 2).texts(&buffer), ["two", "three"]);
        assert_eq!(
            Region::of_rows(&buffer, 0, 99).parts().len(),
            3,
            "clamped, and never past the end"
        );
    }

    #[test]
    fn a_rewrite_that_lengthens_the_text_still_lands_every_part() {
        let mut buffer = buffer("ab\ncd\nef\n");
        let region = Region::of_rows(&buffer, 0, 2);
        region.rewrite(&mut buffer, |text| format!("<{text}>"));
        assert_eq!(buffer.rope().to_string(), "<ab>\n<cd>\n<ef>\n");
    }

    #[test]
    fn a_position_is_carried_across_what_a_rewrite_did() {
        let mut buffer = buffer("ab\ncd\n");
        let region = Region::of_rows(&buffer, 0, 0);
        let edits = region.rewrite(&mut buffer, |text| format!("<{text}>"));
        // `c` was at 3 and is at 5 now.
        assert_eq!(Region::carry(&edits, 3), 5);
    }

    #[test]
    fn a_regions_text_is_spelled_the_way_its_shape_pastes_back() {
        let buffer = buffer("ab\ncd\n");
        assert_eq!(
            Region::of_rows(&buffer, 0, 1).text(&buffer),
            "ab\ncd\n",
            "a linewise entry always ends in one"
        );
        let block = Region::of(&buffer, &sels(&[(0, 3)]), Shape::Block, false);
        assert_eq!(block.text(&buffer), "a\nc", "and a rectangle never does");
    }

    #[test]
    fn an_empty_region_says_so() {
        let buffer = buffer("\n\n");
        assert!(Region::of_rows(&buffer, 0, 1).is_empty(), "rows with no text on them");
        assert!(Region::empty(Shape::Chars).is_empty());
    }
}
