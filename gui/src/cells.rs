//! A row as cells, and everything that paints over one.
//!
//! The terminal builds a row as styled spans and splits them at every
//! boundary a highlight lands on; this frontend builds it as one cell per
//! display column and paints cells, which is the same arithmetic without the
//! splitting. Nothing here knows gpui beyond [`Hsla`], the colour a text run
//! takes, so it can be tested without a window. The passes — decorations
//! under, search, selection, decorations over, inline labels, the horizontal
//! scroll, the fills — are `tui::render::render_window`'s, in its order, and
//! `docs/specs/decorations.md` says why that order.

use gpui::Hsla;

use bi::decoration::{Decoration, Layer};
use bi::indent::{char_width, display_col, glyph};
use bi::syntax::{Span as HlSpan, Syntax};
use bi::theme::{Color as ThemeColor, Style as ThemeStyle, Theme};

/// How a cell is drawn: a theme style resolved over the base colours, in the
/// terms a text run takes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub fg: Hsla,
    pub bg: Option<Hsla>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Look {
    pub fn plain(fg: Hsla) -> Self {
        Self { fg, bg: None, bold: false, italic: false, underline: false }
    }

    /// A theme style laid over this one: what it names wins, what it leaves
    /// unsaid stays — ratatui's `patch`, which is what the terminal does to
    /// a span a selection or a decoration lands on.
    pub fn styled(self, s: ThemeStyle) -> Self {
        let out = Self {
            fg: s.fg.map(color).unwrap_or(self.fg),
            bg: s.bg.map(color).or(self.bg),
            bold: self.bold || s.bold,
            italic: self.italic || s.italic,
            underline: self.underline || s.underline,
        };
        if s.reverse { out.reversed_over(self.bg) } else { out }
    }

    /// The cursor: foreground and background swapped, with `bg` standing in
    /// for a background this cell never named.
    pub fn reversed(self, bg: Hsla) -> Self {
        self.reversed_over(Some(bg))
    }

    fn reversed_over(self, fallback: Option<Hsla>) -> Self {
        let bg = self.bg.or(fallback).unwrap_or(self.fg);
        Self { fg: bg, bg: Some(self.fg), ..self }
    }
}

/// A theme colour as gpui's.
///
/// This function is the whole of what `gui/` knows about colour that `tui/`
/// does not: a terminal is handed the ANSI names and its own palette
/// answers, but a window has no palette to defer to, so the sixteen names
/// and the 256-colour cube get xterm's defaults.
pub fn color(c: ThemeColor) -> Hsla {
    let hex = match c {
        ThemeColor::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
        ThemeColor::Ansi(name) => ANSI[name as usize],
        ThemeColor::Indexed(n) => indexed(n),
    };
    gpui::rgb(hex).into()
}

/// xterm's sixteen, in `theme::Ansi`'s order.
const ANSI: [u32; 16] = [
    0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
    0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
];

fn indexed(n: u8) -> u32 {
    const STEPS: [u32; 6] = [0, 95, 135, 175, 215, 255];
    match n {
        0..=15 => ANSI[n as usize],
        16..=231 => {
            let n = n as u32 - 16;
            (STEPS[(n / 36) as usize] << 16)
                | (STEPS[(n / 6 % 6) as usize] << 8)
                | STEPS[(n % 6) as usize]
        }
        232..=255 => {
            let v = 8 + 10 * (n as u32 - 232);
            (v << 16) | (v << 8) | v
        }
    }
}

/// One cell of a row: a character, how to draw it, and how many columns it
/// takes — two for a CJK char, none for a combining mark, which rides on the
/// cell before it. The columns are what every pass here speaks in, and they
/// have to agree with `indent::display_col`, which is what the core speaks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub look: Look,
    pub width: usize,
}

impl Cell {
    fn blank(look: Look) -> Self {
        Self { ch: ' ', look, width: 1 }
    }
}

/// A line as cells: tabs to the next stop, control characters as `^X`, the
/// C1 controls dropped — `tui::render::cells`, from column zero — with each
/// character's look carried onto every cell it became.
///
/// `looks` is one per char of `text`, or empty for a line with no
/// highlights; a short list means the rest is `base`.
pub fn expand(text: &str, looks: &[Look], base: Look, tab: usize) -> Vec<Cell> {
    let tab = tab.max(1);
    let mut out: Vec<Cell> = Vec::with_capacity(text.len());
    let mut col = 0usize;
    for (i, ch) in text.chars().enumerate() {
        let look = looks.get(i).copied().unwrap_or(base);
        if ch == '\t' {
            let n = tab - (col % tab);
            out.extend(std::iter::repeat_n(Cell::blank(look), n));
            col += n;
        } else if let Some([a, b]) = glyph(ch) {
            out.push(Cell { ch: a, look, width: 1 });
            out.push(Cell { ch: b, look, width: 1 });
            col += 2;
        } else if ch.is_control() {
        } else {
            let width = char_width(ch);
            out.push(Cell { ch, look, width });
            col += width;
        }
    }
    out
}

/// [`expand`] with no looks, for text that is all one colour.
pub fn cells(text: &str, base: Look, tab: usize) -> Vec<Cell> {
    expand(text, &[], base, tab)
}

/// The look of every char on one line, from the parse tree's spans over it.
///
/// `spans` are byte ranges in the whole buffer; `line_start` is the line's.
/// The first span to claim a byte keeps it, as in `tui::render::styled_line`.
pub fn looks(
    raw: &str,
    line_start: usize,
    spans: &[HlSpan],
    syntax: &Syntax,
    theme: &Theme,
    base: Look,
) -> Vec<Look> {
    let mut out = vec![base; raw.chars().count()];
    let bytes: Vec<usize> = raw.char_indices().map(|(i, _)| i).collect();
    let char_at = |byte: usize| bytes.partition_point(|&b| b < byte);
    let mut pos = 0usize;
    for span in spans {
        let start = span.start_byte.saturating_sub(line_start).max(pos);
        let end = span.end_byte.saturating_sub(line_start).min(raw.len());
        if start >= raw.len() {
            break;
        }
        if end <= start {
            continue;
        }
        let Some(style) = theme.style(syntax.capture_name(span.capture)) else { continue };
        for look in &mut out[char_at(start)..char_at(end)] {
            *look = look.styled(style);
        }
        pos = end;
    }
    out
}

/// One row being painted: its cells, and the facts every pass needs about
/// the line they came from.
pub struct Row<'a> {
    pub cells: Vec<Cell>,
    pub row: usize,
    pub raw: &'a str,
    /// Char offset of the start of the row, for the decorations anchored to
    /// text rather than to columns.
    pub start: usize,
    pub tab: usize,
}

impl Row<'_> {
    /// Columns the cells cover.
    pub fn width(&self) -> usize {
        self.cells.iter().map(|c| c.width).sum()
    }

    /// Repaints the cells in `cols`, leaving what the style does not name.
    ///
    /// A cell is painted when any of its columns fall in the range; a
    /// zero-width cell goes with the cell before it, as
    /// `tui::render::paint_range` has it.
    pub fn paint(&mut self, cols: std::ops::Range<usize>, style: ThemeStyle) {
        let mut at = 0usize;
        for cell in &mut self.cells {
            let (w, start) = (cell.width, at);
            at += w;
            let inside = if w == 0 {
                start > cols.start && start < cols.end
            } else {
                start < cols.end && start + w > cols.start
            };
            if inside {
                cell.look = cell.look.styled(style);
            }
        }
    }

    /// Splits at a display column: the cells before it, padded with blanks
    /// when the row is shorter, and the cells from it on. A wide cell across
    /// the boundary goes with the right-hand side, minus the columns it lost.
    fn split(cells: Vec<Cell>, col: usize, pad: Look) -> (Vec<Cell>, Vec<Cell>) {
        let mut left = Vec::with_capacity(cells.len());
        let mut right = Vec::new();
        let mut at = 0usize;
        for cell in cells {
            let end = at + cell.width;
            if end <= col && (cell.width > 0 || at < col) {
                left.push(cell);
            } else if at < col {
                // Straddling: its first column is gone, what remains is a
                // blank of the width left over.
                right.push(Cell { ch: ' ', look: cell.look, width: end - col });
            } else {
                right.push(cell);
            }
            at = end;
        }
        while at < col {
            left.push(Cell::blank(pad));
            at += 1;
        }
        (left, right)
    }

    /// Draws `text` over the cells at `col`, replacing as many as it is wide.
    /// The row comes out the same width it went in.
    pub fn overlay(&mut self, col: usize, text: &str, style: ThemeStyle, base: Look) {
        let over = cells(text, base.styled(style), 8);
        let width: usize = over.iter().map(|c| c.width).sum();
        let (mut out, rest) = Self::split(std::mem::take(&mut self.cells), col, base);
        let (_replaced, right) = Self::split(rest, width, base);
        out.extend(over);
        out.extend(right);
        self.cells = out;
    }

    /// Inserts `text` at `col`, pushing the rest of the row right.
    pub fn insert(&mut self, col: usize, text: &str, style: ThemeStyle, base: Look) {
        let (mut out, right) = Self::split(std::mem::take(&mut self.cells), col, base);
        out.extend(cells(text, base.styled(style), self.tab));
        out.extend(right);
        self.cells = out;
    }

    /// Everything a decoration does to this row, for one layer.
    pub fn decorate(&mut self, decorations: &[Decoration], layer: Layer, base: Look) {
        for decoration in decorations.iter().filter(|d| d.layer() == layer) {
            match decoration {
                Decoration::Overlay { row, col, text, style, .. } if *row == self.row => {
                    self.overlay(*col, text, *style, base);
                }
                Decoration::Eol { row, text, style } if *row == self.row => {
                    self.cells.extend(cells(text, base.styled(*style), self.tab));
                }
                Decoration::Repaint { range, style, .. } => {
                    let chars = self.raw.chars().count();
                    let (from, to) = (range.start, range.end);
                    if to <= self.start || from >= self.start + chars {
                        continue;
                    }
                    let from = display_col(self.raw, from.saturating_sub(self.start), self.tab);
                    let to = display_col(self.raw, (to - self.start).min(chars), self.tab);
                    self.paint(from..to, *style);
                }
                _ => {}
            }
        }
    }

    /// Every inline label on this row, left to right.
    ///
    /// Sorted by column and applied with a running shift, so the columns the
    /// decorations name are all columns of the *original* row. The sort is
    /// stable, so two labels wanting one column come out side by side in the
    /// order they were produced.
    pub fn insert_inline(&mut self, decorations: &[Decoration], base: Look) {
        let mut mine: Vec<(usize, &str, ThemeStyle)> = decorations
            .iter()
            .filter_map(|d| match d {
                Decoration::Inline { row, col, text, style } if *row == self.row => {
                    Some((*col, text.as_str(), *style))
                }
                _ => None,
            })
            .collect();
        mine.sort_by_key(|&(col, _, _)| col);
        let mut shift = 0;
        for (col, text, style) in mine {
            self.insert(col + shift, text, style, base);
            shift += text.chars().map(char_width).sum::<usize>();
        }
    }

    /// The horizontal scroll: the first `left` columns go.
    pub fn scroll(&mut self, left: usize, base: Look) {
        if left > 0 {
            let (_hidden, visible) = Self::split(std::mem::take(&mut self.cells), left, base);
            self.cells = visible;
        }
    }

    /// Paints `bg` behind the row from column `from` on, and pads it to
    /// `width`, so the highlight reaches the edge of the pane. Only where
    /// nothing is painted yet: a selection on the cursor's own line has
    /// already claimed those cells, and the cursor line must not paint over
    /// it. `None` is a theme that asked for no such highlight.
    pub fn fill(&mut self, bg: Option<ThemeColor>, from: usize, width: usize, base: Look) {
        let Some(bg) = bg.map(color) else { return };
        let mut at = 0usize;
        for cell in &mut self.cells {
            if at >= from && cell.look.bg.is_none() {
                cell.look.bg = Some(bg);
            }
            at += cell.width;
        }
        let filled = Look { bg: Some(bg), ..base };
        while at < width {
            self.cells.push(Cell::blank(filled));
            at += 1;
        }
    }

    /// Only the first `width` columns.
    pub fn clip(&mut self, width: usize, base: Look) {
        if self.width() > width {
            let (kept, _) = Self::split(std::mem::take(&mut self.cells), width, base);
            self.cells = kept;
        }
    }
}

/// How far the inline labels on `row` push the cell at `col` to the right.
///
/// A label at the cursor's own column goes *before* the cursor, because it
/// points at the character the cursor is on and would otherwise be under it.
pub fn inline_shift(decorations: &[Decoration], row: usize, col: usize) -> usize {
    decorations
        .iter()
        .filter_map(|d| match d {
            Decoration::Inline { row: at, col: c, text, .. } if *at == row && *c <= col => {
                Some(text.chars().map(char_width).sum::<usize>())
            }
            _ => None,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(cells: &[Cell]) -> String {
        cells.iter().map(|c| c.ch).collect()
    }

    fn base() -> Look {
        Look::plain(gpui::white())
    }

    fn row<'a>(raw: &'a str, tab: usize) -> Row<'a> {
        Row { cells: cells(raw, base(), tab), row: 0, raw, start: 0, tab }
    }

    #[test]
    fn the_cube_and_the_ramp_are_xterms() {
        assert_eq!(indexed(16), 0x000000);
        assert_eq!(indexed(196), 0xff0000, "the cube's red corner");
        assert_eq!(indexed(231), 0xffffff);
        assert_eq!(indexed(232), 0x080808);
        assert_eq!(indexed(255), 0xeeeeee);
        assert_eq!(indexed(9), ANSI[9]);
    }

    #[test]
    fn cells_expand_tabs_and_name_controls() {
        assert_eq!(text(&cells("a\tb", base(), 4)), "a   b");
        assert_eq!(text(&cells("\x01", base(), 4)), "^A");
        assert_eq!(
            text(&cells("x\u{85}y", base(), 4)),
            "xy",
            "a C1 control has no glyph, no width"
        );
    }

    /// A tab is one char with one look, and every cell it becomes keeps it.
    #[test]
    fn a_look_rides_onto_every_cell_its_char_became() {
        let red = Look::plain(gpui::red());
        let cells = expand("\tx", &[red, base()], base(), 4);
        assert_eq!(cells.len(), 5);
        assert!(cells[..4].iter().all(|c| c.look == red));
        assert_eq!(cells[4].look, base());
    }

    #[test]
    fn a_reversed_look_swaps_its_colours_and_the_cursor_is_one() {
        let (fg, bg) = (gpui::white(), gpui::black());
        let look = Look::plain(fg).reversed(bg);
        assert_eq!((look.fg, look.bg), (bg, Some(fg)));
        // Reversing a cell that had its own background keeps that background
        // as the new foreground, so a highlighted cell under the cursor still
        // reads as that highlight.
        let own = Look { bg: Some(gpui::red()), ..Look::plain(fg) }.reversed(bg);
        assert_eq!((own.fg, own.bg), (gpui::red(), Some(fg)));
    }

    /// The columns a wide char takes agree with the core's `display_col`,
    /// which is what a selection's edges are expressed in.
    #[test]
    fn a_wide_char_is_two_columns_and_paints_as_one_cell() {
        let mut r = row("a日b", 4);
        assert_eq!(r.width(), 4);
        assert_eq!(display_col("a日b", 2, 4), 3, "the core agrees");
        r.paint(2..3, ThemeStyle::fg(ThemeColor::Rgb(255, 0, 0)));
        assert_eq!(r.cells[1].look.fg, gpui::red().into(), "half of it in the range paints it");
        assert_eq!(r.cells[2].look, base(), "the b after it is untouched");
    }

    #[test]
    fn an_overlay_keeps_the_width_and_an_insert_grows_it() {
        let mut r = row("abcdef", 4);
        r.overlay(2, "XY", ThemeStyle::default(), base());
        assert_eq!(text(&r.cells), "abXYef");
        r.insert(2, "--", ThemeStyle::default(), base());
        assert_eq!(text(&r.cells), "ab--XYef");
    }

    /// An indent guide on a blank row lands past the end of the text, which
    /// is exactly what the padding in `split` is for.
    #[test]
    fn an_overlay_past_the_end_pads_with_blanks() {
        let mut r = row("", 4);
        r.overlay(4, "│", ThemeStyle::default(), base());
        assert_eq!(text(&r.cells), "    │");
    }

    #[test]
    fn scroll_drops_columns_and_fill_paints_only_the_unclaimed() {
        let mut r = row("abcdef", 4);
        r.scroll(2, base());
        assert_eq!(text(&r.cells), "cdef");
        r.paint(0..1, ThemeStyle { bg: Some(ThemeColor::Rgb(0, 0, 255)), ..ThemeStyle::default() });
        r.fill(Some(ThemeColor::Rgb(255, 0, 0)), 0, 6, base());
        assert_eq!(r.width(), 6, "padded to the pane");
        assert_eq!(r.cells[0].look.bg, Some(gpui::blue().into()), "the selection kept its cell");
        assert_eq!(r.cells[1].look.bg, Some(gpui::red().into()));
        assert_eq!(r.cells[5].look.bg, Some(gpui::red().into()), "and so is the padding");
    }

    #[test]
    fn inline_labels_shift_each_other_and_the_cursor() {
        let decorations = vec![
            Decoration::Inline { row: 0, col: 3, text: "b".into(), style: ThemeStyle::default() },
            Decoration::Inline { row: 0, col: 1, text: "a".into(), style: ThemeStyle::default() },
        ];
        let mut r = row("wxyz", 4);
        r.insert_inline(&decorations, base());
        assert_eq!(text(&r.cells), "waxybz", "columns of the original row");
        assert_eq!(inline_shift(&decorations, 0, 3), 2, "a cursor at 3 sits after both");
        assert_eq!(inline_shift(&decorations, 0, 2), 1);
    }
}
