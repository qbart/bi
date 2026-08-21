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
use crate::selection::Selections;

/// How a region was meant.
///
/// One enum for three questions that were three enums: what kind of selection
/// is on screen, how text in a register was taken, and what `.` repeats. They
/// were always the same three answers, and keeping them apart meant keeping
/// them in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A span within a line. Pastes inline.
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

/// One span of one row. Never crosses a line terminator.
///
/// Row-clipping is what makes charwise, linewise and blockwise the same data:
/// they differ only in how the list was built, so every rewrite over a region
/// is one loop rather than three arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

/// The region an operation applies to: a shape, and one span per row.
///
/// Spans are sorted and disjoint. Empty ones are kept — a rectangle is a
/// rectangle even where the text is not, and dropping the short rows would
/// lose the shape a yanked block has to keep.
///
/// Not `Selections`, which sorts *and merges*: two rows whose spans meet at a
/// line end would silently weld together. A region has no merge invariant,
/// because it is not a set of cursors — it is the answer to one question,
/// thrown away afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    shape: Shape,
    spans: Vec<Span>,
}

impl Region {
    /// From what is on screen: every selection, shaped by `shape`.
    ///
    /// Multi-cursor needs no arm of its own. Three collapsed selections are
    /// three one-character spans, and every operation over them is the same
    /// loop as everything else.
    ///
    /// `to_eol` is blockwise `$` — the right edge is each row's line end.
    pub fn of(buffer: &Buffer, selections: &Selections, shape: Shape, to_eol: bool) -> Self {
        let spans = match shape {
            Shape::Block => block_spans(buffer, selections, to_eol),
            _ => selections
                .all()
                .iter()
                .flat_map(|selection| {
                    let (lo, hi) = match shape {
                        Shape::Lines => {
                            buffer.line_range(selection.range().0, selection.range().1, false)
                        }
                        _ => selection.inclusive_range(buffer.rope().len_chars()),
                    };
                    split_rows(buffer, lo, hi.max(lo))
                })
                .collect(),
        };
        Self { shape, spans }
    }

    /// Whole rows `first..=last`, which is what a `:` line's addresses name.
    pub fn rows(buffer: &Buffer, first: usize, last: usize) -> Self {
        let last = last.min(buffer.line_count().saturating_sub(1));
        let spans = (first..=last.max(first))
            .filter(|&row| row < buffer.line_count())
            .map(|row| {
                let start = buffer.rope().line_to_char(row);
                Span { row, start, end: start + buffer.line_len(row) }
            })
            .collect();
        Self { shape: Shape::Lines, spans }
    }

    /// From explicit char ranges — what a text object or a search gives.
    ///
    /// Each range is split at row boundaries, so the result obeys the same
    /// one-span-per-row rule as everything else here.
    pub fn spanning(
        buffer: &Buffer,
        shape: Shape,
        ranges: impl IntoIterator<Item = (usize, usize)>,
    ) -> Self {
        let mut spans: Vec<Span> = ranges
            .into_iter()
            .flat_map(|(start, end)| split_rows(buffer, start, end.max(start)))
            .collect();
        spans.sort_by_key(|span| (span.start, span.end));
        Self { shape, spans }
    }

    /// A region covering nothing. What a command with nothing to act on gets.
    pub fn empty(shape: Shape) -> Self {
        Self { shape, spans: Vec::new() }
    }

    /// The same rows, whole.
    ///
    /// What a command that can only work in whole lines does with a region
    /// that is not made of them — `:m` cannot move half a row. One widening,
    /// in one place, so that a command handed a shape it cannot use says so
    /// rather than inventing an answer.
    pub fn to_rows(&self, buffer: &Buffer) -> Self {
        match self.row_range() {
            Some((first, last)) => Self::rows(buffer, first, last),
            None => Self::empty(Shape::Lines),
        }
    }

    /// Whether [`Region::to_rows`] would change anything.
    pub fn is_rows(&self, buffer: &Buffer) -> bool {
        self.shape == Shape::Lines
            && self.spans.iter().all(|span| {
                let start = buffer.rope().line_to_char(span.row);
                span.start == start && span.end == start + buffer.line_len(span.row)
            })
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Whether there is any text here. A rectangle over three empty rows has
    /// spans and no text, and every operation over it is a no-op.
    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|span| span.is_empty())
    }

    /// The first character the region covers, or the top-left corner of a
    /// rectangle. Where the cursor lands after most rewrites.
    pub fn start(&self) -> usize {
        self.spans.first().map_or(0, |span| span.start)
    }

    /// The rows the region touches, first and last.
    pub fn row_range(&self) -> Option<(usize, usize)> {
        Some((self.spans.first()?.row, self.spans.last()?.row))
    }

    /// How many rows carry text.
    pub fn filled_rows(&self) -> usize {
        self.spans.iter().filter(|span| !span.is_empty()).count()
    }

    /// The text of each span, one per row, in order.
    pub fn texts(&self, buffer: &Buffer) -> Vec<String> {
        self.spans.iter().map(|span| buffer.slice(span.start, span.end)).collect()
    }

    /// The whole region as one string, ready for a register.
    ///
    /// A rectangle joins its rows with `\n` and stops; lines end in one. This
    /// is the one place the shape decides how text is spelled, so a register
    /// entry and the region it came from cannot disagree.
    pub fn text(&self, buffer: &Buffer) -> String {
        match self.shape {
            Shape::Block => self.texts(buffer).join("\n"),
            Shape::Lines => {
                let mut text = self.texts(buffer).join("\n");
                text.push('\n');
                text
            }
            Shape::Chars => self.texts(buffer).join("\n"),
        }
    }

    /// Rewrites every span through `f`, and hands back the edits it made.
    ///
    /// Last span first: an edit shifts everything below it and nothing above,
    /// so descending order keeps every span's position valid without a
    /// correction pass. Written once here rather than at the four call sites
    /// that each had their own copy of the comment.
    ///
    /// The edits are the buffer's own, so carrying selections across them is
    /// [`Edit::map`] — the same mapping `trim` and `retab` already use, rather
    /// than a third strategy.
    pub fn rewrite(&self, buffer: &mut Buffer, f: impl Fn(&str) -> String) -> Vec<Edit> {
        let base = buffer.pending_edits.len();
        for span in self.spans.iter().rev() {
            if span.is_empty() {
                continue;
            }
            let text = f(&buffer.slice(span.start, span.end));
            buffer.replace_range(span.start, span.end, &text);
        }
        buffer.pending_edits[base..].to_vec()
    }

    /// Cuts every span out, bottom to top.
    pub fn cut(&self, buffer: &mut Buffer) -> Vec<Edit> {
        self.rewrite(buffer, |_| String::new())
    }

    /// Carries a position across what [`Region::rewrite`] did to the buffer.
    pub fn carry(edits: &[Edit], at: usize) -> usize {
        edits.iter().fold(at, |at, edit| edit.map(at))
    }
}

/// One span per row between two char offsets, clipped to each row's content.
fn split_rows(buffer: &Buffer, lo: usize, hi: usize) -> Vec<Span> {
    let first = buffer.row_at(Cursor::at(lo));
    let last = buffer.row_at(Cursor::at(hi));
    (first..=last)
        .map(|row| {
            let start = buffer.rope().line_to_char(row);
            let end = start + buffer.line_len(row);
            Span { row, start: start.max(lo), end: end.min(hi).max(start.max(lo)) }
        })
        .filter(|span| !span.is_empty())
        .collect()
}

/// One span per row the rectangle covers, top to bottom.
///
/// Rows too short to reach the left edge come back empty and *stay in the
/// list*: a block is a rectangle even where the text is not.
fn block_spans(buffer: &Buffer, selections: &Selections, to_eol: bool) -> Vec<Span> {
    let (lo, hi) = selections.primary().range();
    let (first, last) = (buffer.row_at(Cursor::at(lo)), buffer.row_at(Cursor::at(hi)));
    (first..=last).map(|row| block_span_at(buffer, selections, to_eol, row)).collect()
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
    use crate::selection::Selection;

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

    fn slices(region: &Region, buffer: &Buffer) -> Vec<String> {
        region.texts(buffer)
    }

    #[test]
    fn a_charwise_region_is_the_selected_characters() {
        let buffer = buffer("hello world\n");
        let region = Region::of(&buffer, &sels(&[(0, 4)]), Shape::Chars, false);
        assert_eq!(slices(&region, &buffer), ["hello"], "the head's char is included");
    }

    #[test]
    fn a_charwise_region_over_two_rows_is_one_span_each() {
        let buffer = buffer("one\ntwo\n");
        let region = Region::of(&buffer, &sels(&[(1, 5)]), Shape::Chars, false);
        assert_eq!(slices(&region, &buffer), ["ne", "tw"], "and never the terminator");
    }

    #[test]
    fn a_linewise_region_is_whole_rows_without_the_terminator() {
        let buffer = buffer("one\ntwo\nthree\n");
        let region = Region::of(&buffer, &sels(&[(1, 5)]), Shape::Lines, false);
        assert_eq!(slices(&region, &buffer), ["one", "two"]);
    }

    #[test]
    fn a_blockwise_region_is_the_columns_and_nothing_else() {
        let buffer = buffer("let ALPHA = 1;\nlet BETA  = 2;\nlet GAMMA = 3;\n");
        // Columns 4..=8 of three rows.
        let region = Region::of(&buffer, &sels(&[(4, 15 + 15 + 8)]), Shape::Block, false);
        assert_eq!(slices(&region, &buffer), ["ALPHA", "BETA ", "GAMMA"]);
    }

    #[test]
    fn a_row_too_short_for_the_rectangle_keeps_its_empty_span() {
        let buffer = buffer("aaaa\nbb\ncccc\n");
        // Columns 2..=3 of three rows; the middle row stops at 2.
        let region = Region::of(&buffer, &sels(&[(2, 8 + 3)]), Shape::Block, false);
        assert_eq!(slices(&region, &buffer), ["aa", "", "cc"], "a rectangle keeps its shape");
        assert_eq!(region.spans().len(), 3);
        assert_eq!(region.filled_rows(), 2);
    }

    #[test]
    fn ragged_right_takes_every_row_to_its_own_end() {
        let buffer = buffer("aaaa\nbb\ncccccc\n");
        let region = Region::of(&buffer, &sels(&[(1, 8 + 1)]), Shape::Block, true);
        assert_eq!(slices(&region, &buffer), ["aaa", "b", "ccccc"]);
    }

    /// Multi-cursor needs no arm: three cursors are three one-char spans.
    #[test]
    fn every_selection_contributes_its_own_span() {
        let buffer = buffer("one two six\n");
        let region = Region::of(&buffer, &sels(&[(0, 2), (4, 6), (8, 10)]), Shape::Chars, false);
        assert_eq!(slices(&region, &buffer), ["one", "two", "six"]);
    }

    #[test]
    fn a_row_region_is_the_lines_it_names() {
        let buffer = buffer("one\ntwo\nthree\n");
        assert_eq!(slices(&Region::rows(&buffer, 1, 2), &buffer), ["two", "three"]);
        assert_eq!(
            Region::rows(&buffer, 0, 99).spans().len(),
            3,
            "clamped, and never past the end"
        );
    }

    #[test]
    fn a_rewrite_that_lengthens_the_text_still_lands_every_span() {
        let mut buffer = buffer("ab\ncd\nef\n");
        let region = Region::of(&buffer, &sels(&[(0, 7)]), Shape::Lines, false);
        region.rewrite(&mut buffer, |text| format!("<{text}>"));
        assert_eq!(buffer.rope().to_string(), "<ab>\n<cd>\n<ef>\n");
    }

    #[test]
    fn a_position_is_carried_across_what_a_rewrite_did() {
        let mut buffer = buffer("ab\ncd\n");
        let region = Region::rows(&buffer, 0, 0);
        let edits = region.rewrite(&mut buffer, |text| format!("<{text}>"));
        // `c` was at 3 and is at 5 now.
        assert_eq!(Region::carry(&edits, 3), 5);
    }

    #[test]
    fn a_regions_text_is_spelled_the_way_its_shape_pastes_back() {
        let buffer = buffer("ab\ncd\n");
        let rows = Region::rows(&buffer, 0, 1);
        assert_eq!(rows.text(&buffer), "ab\ncd\n", "a linewise entry always ends in one");
        let block = Region::of(&buffer, &sels(&[(0, 3)]), Shape::Block, false);
        assert_eq!(block.text(&buffer), "a\nc", "and a rectangle never does");
    }

    #[test]
    fn an_empty_region_says_so() {
        let buffer = buffer("\n\n");
        assert!(Region::rows(&buffer, 0, 1).is_empty(), "rows with no text on them");
        assert!(Region::empty(Shape::Chars).is_empty());
    }
}
