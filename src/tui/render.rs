//! Rendering.
//!
//! Only the visible viewport is ever touched — the cost of a frame is bounded
//! by terminal height, not buffer size. Keep it that way.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use bi::buffer::Cursor;
use bi::config::Options;
use bi::decoration::{Decoration, Layer};
use bi::editor::{Editor, Mode, Pane, VisualKind};
use bi::indent::{display_col, expand_tabs};
use bi::picker::{Picker, PickerKind};
use bi::selection::Selections;
use bi::syntax::{Span as HlSpan, Syntax};
use bi::theme::{Ansi, Color as ThemeColor, Style as ThemeStyle, Theme, Ui};
use bi::tree::{ClipMode, Clipboard, Kind, Row as TreeRow, Tree};
use bi::window::{Chrome, ContentKind, Rect as CoreRect, WindowId};

/// A theme colour, in the one spelling a terminal understands.
///
/// This function is the whole of what `tui/` knows that a GUI frontend would
/// not — the rest of the palette is `bi::theme`, which names colours and never
/// draws them.
fn color(c: ThemeColor) -> Color {
    match c {
        ThemeColor::Indexed(n) => Color::Indexed(n),
        ThemeColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
        ThemeColor::Ansi(name) => match name {
            Ansi::Black => Color::Black,
            Ansi::Red => Color::Red,
            Ansi::Green => Color::Green,
            Ansi::Yellow => Color::Yellow,
            Ansi::Blue => Color::Blue,
            Ansi::Magenta => Color::Magenta,
            Ansi::Cyan => Color::Cyan,
            Ansi::Gray => Color::Gray,
            Ansi::DarkGray => Color::DarkGray,
            Ansi::LightRed => Color::LightRed,
            Ansi::LightGreen => Color::LightGreen,
            Ansi::LightYellow => Color::LightYellow,
            Ansi::LightBlue => Color::LightBlue,
            Ansi::LightMagenta => Color::LightMagenta,
            Ansi::LightCyan => Color::LightCyan,
            Ansi::White => Color::White,
        },
    }
}

/// A theme style as ratatui's.
fn tui(s: ThemeStyle) -> Style {
    let mut out = Style::default();
    if let Some(fg) = s.fg {
        out = out.fg(color(fg));
    }
    if let Some(bg) = s.bg {
        out = out.bg(color(bg));
    }
    for (on, modifier) in [
        (s.bold, Modifier::BOLD),
        (s.italic, Modifier::ITALIC),
        (s.underline, Modifier::UNDERLINED),
        (s.reverse, Modifier::REVERSED),
    ] {
        if on {
            out = out.add_modifier(modifier);
        }
    }
    out
}

/// The base every cell starts from: the theme's background and foreground, or
/// nothing at all where the theme declined to name them.
///
/// A theme that names neither is invisible here, which is the point — bi drew
/// on the terminal's own colours before it had themes, and `ansi` still does.
fn base_style(ui: &Ui) -> Style {
    let mut out = Style::default();
    if let Some(fg) = ui.foreground {
        out = out.fg(color(fg));
    }
    if let Some(bg) = ui.background {
        out = out.bg(color(bg));
    }
    out
}

/// Paints the theme's base over `area`.
///
/// Called for the whole frame before anything draws, and again after `Clear`,
/// which resets cells to the terminal's own and would otherwise punch a hole
/// in the background wherever the picker opens.
fn paint_base(frame: &mut Frame, area: Rect, ui: &Ui) {
    let base = base_style(ui);
    if base != Style::default() {
        frame.buffer_mut().set_style(area, base);
    }
}

/// Repaints the background of the columns in `cols` within an already-built
/// line, leaving the foreground alone.
///
/// Spans are split at the range's edges rather than styled whole, because a
/// selection almost never lines up with a syntax span.
fn paint_range(
    spans: Vec<Span<'static>>,
    cols: std::ops::Range<usize>,
    over: ThemeStyle,
) -> Vec<Span<'static>> {
    let over = tui(over);
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    let mut col = 0usize;
    for span in spans {
        let text = span.content.to_string();
        let width = text.chars().count();
        let (start, end) = (col, col + width);
        col = end;

        if end <= cols.start || start >= cols.end {
            out.push(span);
            continue;
        }
        let lo = cols.start.saturating_sub(start);
        let hi = (cols.end - start).min(width);
        let take =
            |from: usize, to: usize| -> String { text.chars().take(to).skip(from).collect() };

        if lo > 0 {
            out.push(Span::styled(take(0, lo), span.style));
        }
        out.push(Span::styled(take(lo, hi), span.style.patch(over)));
        if hi < width {
            out.push(Span::styled(take(hi, width), span.style));
        }
    }
    out
}

/// Splits a built line at a display column.
///
/// Pads the left half when the line is shorter than the column, which is how a
/// decoration lands past the end of a line — an indent guide on a blank row is
/// exactly that case.
fn split_at_col(spans: Vec<Span<'static>>, col: usize) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut at = 0usize;
    for span in spans {
        let text = span.content.to_string();
        let width = text.chars().count();
        if at + width <= col {
            at += width;
            left.push(span);
        } else if at >= col {
            at += width;
            right.push(span);
        } else {
            let cut = col - at;
            at += width;
            left.push(Span::styled(text.chars().take(cut).collect::<String>(), span.style));
            right.push(Span::styled(text.chars().skip(cut).collect::<String>(), span.style));
        }
    }
    if at < col && right.is_empty() {
        left.push(Span::raw(" ".repeat(col - at)));
    }
    (left, right)
}

/// Draws `text` over the cells at `col`, replacing as many as it is wide.
///
/// The line comes out the same length it went in, so nothing after the overlay
/// shifts — which is the whole difference between an overlay and inserting.
fn overlay(
    spans: Vec<Span<'static>>,
    col: usize,
    text: &str,
    style: ThemeStyle,
) -> Vec<Span<'static>> {
    let (mut out, rest) = split_at_col(spans, col);
    let (_replaced, right) = split_at_col(rest, text.chars().count());
    out.push(Span::styled(text.to_string(), tui(style)));
    out.extend(right);
    out
}

/// Inserts every inline label on one row, left to right.
///
/// Sorted by column and applied with a running shift, so the columns the
/// decorations name are all columns of the *original* row and none of them has
/// to know what the ones before it did. The sort is stable, which is what makes
/// two labels wanting one column come out in the order they were produced —
/// two cells side by side rather than one on top of the other.
fn insert_inline(
    mut spans: Vec<Span<'static>>,
    line: &Row,
    decorations: &[Decoration],
) -> Vec<Span<'static>> {
    let mut mine: Vec<(usize, &str, ThemeStyle)> = decorations
        .iter()
        .filter_map(|d| match d {
            Decoration::Inline { row, col, text, style } if *row == line.row => {
                Some((*col, text.as_str(), *style))
            }
            _ => None,
        })
        .collect();
    if mine.is_empty() {
        return spans;
    }
    mine.sort_by_key(|&(col, _, _)| col);

    let mut shift = 0;
    for (col, text, style) in mine {
        let (mut out, right) = split_at_col(spans, col + line.gutter + shift);
        out.push(Span::styled(text.to_string(), tui(style)));
        out.extend(right);
        spans = out;
        shift += text.chars().count();
    }
    spans
}

/// How far the inline labels on `row` push the cell at `col` to the right.
///
/// A label at the cursor's own column goes *before* the cursor, because it
/// points at the character the cursor is on and would otherwise be under it.
fn inline_shift(decorations: &[Decoration], row: usize, col: usize) -> usize {
    decorations
        .iter()
        .filter_map(|d| match d {
            Decoration::Inline { row: at, col: c, text, .. } if *at == row && *c <= col => {
                Some(text.chars().count())
            }
            _ => None,
        })
        .sum()
}

/// One row, as everything that paints over it needs to see it: which row it
/// is, its text, where that text starts in the rope, and where its columns
/// begin on screen.
struct Row<'a> {
    row: usize,
    raw: &'a str,
    /// Char offset of the start of the row, for the decorations anchored to
    /// text rather than to columns.
    start: usize,
    gutter: usize,
    tab: usize,
}

/// Everything a decoration does to one already-built line.
fn decorate(
    mut spans: Vec<Span<'static>>,
    line: &Row,
    decorations: &[Decoration],
    layer: Layer,
) -> Vec<Span<'static>> {
    let Row { row, raw, start: line_start, gutter, tab } = *line;
    for decoration in decorations.iter().filter(|d| d.layer() == layer) {
        match decoration {
            Decoration::Overlay { row: at, col, text, style, .. } if *at == row => {
                spans = overlay(spans, col + gutter, text, *style);
            }
            Decoration::Eol { row: at, text, style } if *at == row => {
                spans.push(Span::styled(text.clone(), tui(*style)));
            }
            Decoration::Repaint { range, style, .. } => {
                let chars = raw.chars().count();
                let (from, to) = (range.start, range.end);
                if to <= line_start || from >= line_start + chars {
                    continue;
                }
                let from = display_col(raw, from.saturating_sub(line_start), tab);
                let to = display_col(raw, (to - line_start).min(chars), tab);
                spans = paint_range(spans, (from + gutter)..(to + gutter), *style);
            }
            _ => {}
        }
    }
    spans
}

/// Paints `bg` behind a line and pads it to `width`, so the highlight reaches
/// the edge of the pane instead of stopping at the last character.
///
/// `None` is a theme that asked for no such highlight, and leaves the line
/// alone rather than painting it a colour nobody chose.
///
/// Span styles are patched rather than replaced — the syntax colours are the
/// foreground and have to survive.
fn fill_line(spans: Vec<Span<'static>>, bg: Option<ThemeColor>, width: usize) -> Line<'static> {
    let Some(bg) = bg.map(color) else { return Line::from(spans) };
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let mut spans: Vec<Span<'static>> = spans
        .into_iter()
        .map(|s| {
            // Only where nothing is painted yet: a selection on the cursor's
            // own line has already claimed those cells, and the cursor line
            // must not paint over it.
            match s.style.bg {
                Some(_) => s,
                None => {
                    let style = s.style.bg(bg);
                    s.style(style)
                }
            }
        })
        .collect();
    spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), Style::default().bg(bg)));
    Line::from(spans)
}

/// Splits one line into styled pieces, expanding tabs as it goes.
///
/// Tab expansion has to happen *inside* the split: expanding first would shift
/// every byte offset the highlight spans are expressed in.
fn styled_line(
    raw: &str,
    line_start: usize,
    spans: &[HlSpan],
    syntax: &Syntax,
    theme: &Theme,
    tab_width: usize,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    let mut pos = 0usize;

    let mut push = |text: &str, style: Style, col: &mut usize| {
        if text.is_empty() {
            return;
        }
        let mut expanded = String::with_capacity(text.len());
        for ch in text.chars() {
            if ch == '\t' {
                let n = tab_width - (*col % tab_width);
                expanded.extend(std::iter::repeat_n(' ', n));
                *col += n;
            } else {
                expanded.push(ch);
                *col += 1;
            }
        }
        out.push(Span::styled(expanded, style));
    };

    for span in spans {
        let start = span.start_byte.saturating_sub(line_start);
        let end = span.end_byte.saturating_sub(line_start);
        if start >= raw.len() {
            break;
        }
        let end = end.min(raw.len());
        if end <= pos || !raw.is_char_boundary(start) || !raw.is_char_boundary(end) {
            continue;
        }
        push(&raw[pos..start], Style::default(), &mut col);
        let named = theme.style(syntax.capture_name(span.capture));
        push(&raw[start..end], named.map(tui).unwrap_or_default(), &mut col);
        pos = end;
    }
    push(&raw[pos..], Style::default(), &mut col);
    out
}

/// Columns the gutter takes: the widest line number plus a space, or none at
/// all when it is off.
///
/// Fixed across modes on purpose — sizing it to the largest *relative* label
/// would make the gutter change width as the cursor moves, sliding every line
/// of the file sideways while you scroll.
fn gutter_width(options: &Options, buffer: &bi::buffer::Buffer) -> usize {
    options.gutter_width(buffer.line_count())
}

/// One column between panes that sit side by side; no rows between stacked
/// ones, since each window's status line already separates those.
///
/// `min_height` is 2 because a pane has to fit a status row and a line of text
/// — a terminal convention, which is why it lives here and not in the core.
const CHROME: Chrome = Chrome { columns: 1, rows: 0, min_width: 8, min_height: 2, tree_width: 30 };

fn to_core(r: Rect) -> CoreRect {
    CoreRect { x: r.x, y: r.y, width: r.width, height: r.height }
}

fn to_tui(r: CoreRect) -> Rect {
    Rect { x: r.x, y: r.y, width: r.width, height: r.height }
}

pub fn render(frame: &mut Frame, ed: &mut Editor, pending: &str) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());

    let panes: Vec<(WindowId, Rect)> =
        ed.layout(to_core(body), CHROME).into_iter().map(|(id, rect)| (id, to_tui(rect))).collect();

    // Every window is told its size before anything is drawn, so scrolling has
    // settled by the time the first pane is formatted. A tree keeps its whole
    // pane: it has no status row, because its first row already names the root
    // and a sidebar cannot spare a line to say so twice.
    for &(id, rect) in &panes {
        let height = match ed.content_kind_of(id) {
            Some(ContentKind::Tree) => rect.height,
            _ => rect.height.saturating_sub(1),
        };
        ed.size_window(id, rect.width as usize, height as usize);
    }

    // Copied out once: `Ui` is `Copy`, and the picker below borrows the
    // editor mutably, which a `&Theme` held across it could not survive.
    let ui = ed.theme().ui;
    // The session's, not a buffer's: the overlay is showing registers and
    // command lines, which belong to no file.
    let tab = ed.session.options.tab_width;
    // The theme's own background, under everything, before anything draws. A
    // theme that named none leaves the terminal's showing through — which is
    // what bi did before it had themes, and what `ansi` still does.
    paint_base(frame, frame.area(), &ui);

    let focus = ed.focus();
    let mut cursor_at = None;

    for &(id, rect) in &panes {
        let tree = ed.content_kind_of(id) == Some(ContentKind::Tree);
        let [body_area, status] = match tree {
            true => [rect, Rect { height: 0, ..rect }],
            false => Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(rect),
        };

        let at = render_window(frame, ed, id, body_area, id == focus);
        if id == focus {
            cursor_at = at;
        }
        if !tree {
            let row = window_status(ed, id, id == focus, status.width);
            frame.render_widget(Paragraph::new(Line::from(row)), status);
        }

        // The rule in the column the layout reserved to the left of this pane.
        if rect.x > body.x {
            let rule = Rect { x: rect.x - 1, y: rect.y, width: 1, height: rect.height };
            let bar: Vec<Line> = (0..rect.height)
                .map(|_| Line::from(Span::styled("│", tui(ed.theme().ui.rule))))
                .collect();
            frame.render_widget(Paragraph::new(bar), rule);
        }
    }

    // Worked out here rather than inside the footer: it caches on `Editor` and
    // so needs the mutable borrow the widget builders do not have.
    let matches = ed.search_count();
    frame.render_widget(status_line(ed, pending, matches, footer.width), footer);

    if matches!(ed.session.mode, Mode::Pick)
        && let Some(picker) = ed.session.picker.as_mut()
    {
        render_picker(frame, picker, body, &ui, tab);
        return;
    }

    match &ed.session.mode {
        Mode::Command(line) => {
            frame.set_cursor_position((footer.x + 1 + line.chars().count() as u16, footer.y));
        }
        // Only the focused window gets it: there is one cursor, and it goes
        // where typing goes.
        _ => {
            if let Some(at) = cursor_at {
                frame.set_cursor_position(at);
            }
        }
    }
}

/// Everything to the left of a row's name: the mark column, the indent and the
/// open/closed marker.
///
/// Split out because the alignment is the part worth guarding, and a
/// `Paragraph` cannot be read back.
fn tree_row_parts(row: &TreeRow, mark: Option<ClipMode>) -> (String, String) {
    // One column, always, so marking something does not shift the tree
    // sideways under the cursor.
    let mark = match mark {
        Some(ClipMode::Copy) => '+',
        Some(ClipMode::Cut) => '~',
        None => ' ',
    };
    let marker = match (row.kind, row.open) {
        (Kind::Dir, true) => "▾ ",
        (Kind::Dir, false) => "▸ ",
        _ => "  ",
    };
    let name = match row.kind {
        Kind::Dir => format!("{}/", row.name),
        Kind::Link => format!("{}@", row.name),
        Kind::File => row.name.clone(),
    };
    (format!("{mark}{}{marker}", "  ".repeat(row.depth)), name)
}

/// One window's tree. Returns where the terminal cursor belongs.
///
/// The glyphs are chosen here rather than in the core, which hands over depth
/// and kind and nothing that looks like anything — README decision #6 for a
/// second subsystem. No gutter and no highlighting: a row is not a line, so it
/// has no line number, and a path has no syntax.
fn render_tree(
    frame: &mut Frame,
    tree: &Tree,
    clipboard: &Clipboard,
    area: Rect,
    focused: bool,
    ui: &Ui,
) -> Option<(u16, u16)> {
    let rows = tree.rows();
    let last = (tree.scroll() + area.height as usize).min(rows.len());
    let mut cursor_at = None;
    let mut lines = Vec::with_capacity(area.height as usize);

    for (index, row) in rows.iter().enumerate().take(last).skip(tree.scroll()) {
        // Per row, so a mixed clipboard shows which of its paths are being
        // copied and which are leaving.
        let mark = clipboard.mode_of(&row.path);
        let (indent, name) = tree_row_parts(row, mark);

        let style = match row.kind {
            Kind::Dir => tui(ui.tree_dir).add_modifier(Modifier::BOLD),
            Kind::Link => tui(ui.tree_link),
            Kind::File => Style::default(),
        };
        let mark_style = match mark {
            Some(ClipMode::Copy) => tui(ui.mark_copy).add_modifier(Modifier::BOLD),
            Some(ClipMode::Cut) => tui(ui.mark_cut).add_modifier(Modifier::BOLD),
            None => Style::default(),
        };

        let spans = vec![Span::styled(indent.clone(), mark_style), Span::styled(name, style)];
        if index != tree.selected() {
            lines.push(Line::from(spans));
            continue;
        }

        // Shown in every tree pane, not only the focused one: unlike a text
        // cursor this is where the *next* Enter goes, so hiding it would make
        // switching to a tree a guess.
        let bg = if focused { ui.selection.bg } else { ui.cursorline.bg };
        lines.push(fill_line(spans, bg, area.width as usize));
        if focused {
            let col = indent.chars().count() as u16;
            cursor_at = Some((
                area.x + col.min(area.width.saturating_sub(1)),
                area.y + lines.len() as u16 - 1,
            ));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
    cursor_at
}

/// One window's text. Returns where the terminal cursor belongs, when this is
/// the window that has it.
fn render_window(
    frame: &mut Frame,
    ed: &Editor,
    id: WindowId,
    text_area: Rect,
    focused: bool,
) -> Option<(u16, u16)> {
    let (text, buffer, syntax, options) = match ed.pane(id)? {
        Pane::Text { text, buffer, syntax, options, .. } => (text, buffer, syntax, options),
        Pane::Tree { tree, .. } => {
            return render_tree(
                frame,
                tree,
                &ed.session.clipboard,
                text_area,
                focused,
                &ed.theme().ui,
            );
        }
    };
    let (scroll, selections) = (text.scroll, &text.selections);

    // The one number the core owns and the renderer used to guess — and this
    // buffer's, not the session's, so a Makefile and the file beside it can
    // disagree about how wide a tab is.
    let tab = options.tab_width;
    let total = buffer.line_count();
    let gutter = gutter_width(options, buffer);
    let cursor = selections.cursor();
    let cursor_row = buffer.row_at(cursor);

    let mut lines = Vec::with_capacity(text_area.height as usize);
    let mut cursor_screen_col = 0;

    // One query for the whole visible range, then partition per line. Bounded
    // by pane height, never by file size.
    let last_row = (scroll + text_area.height as usize).min(total);
    // The same rule, for everything drawn that is not buffer text.
    let decorations = ed.decorations(id, scroll..last_row);
    let highlights = syntax.map(|syntax| {
        let rope = buffer.rope();
        let from = rope.line_to_byte(scroll.min(rope.len_lines()));
        let to = rope.line_to_byte(last_row.min(rope.len_lines()));
        (syntax, syntax.highlights(rope, from..to))
    });

    for row in scroll..last_row {
        let raw = buffer.rope().line(row).to_string();
        let raw = raw.trim_end_matches(['\n', '\r']);

        if row == cursor_row {
            let col = display_col(raw, buffer.col_at(cursor), tab);
            cursor_screen_col = col + inline_shift(&decorations, row, col);
        }

        // A blank cell where a number is not due, so the text stays put.
        let mut spans = match options.number.label_for(row, cursor_row) {
            _ if gutter == 0 => Vec::new(),
            Some(n) => vec![Span::styled(
                format!("{n:>width$} ", width = gutter - 1),
                tui(if row == cursor_row {
                    ed.theme().ui.gutter_current
                } else {
                    ed.theme().ui.gutter
                }),
            )],
            None => vec![Span::raw(" ".repeat(gutter))],
        };
        match &highlights {
            Some((syntax, all)) => {
                let line_start = buffer.rope().line_to_byte(row);
                let line_end = line_start + raw.len();
                let mine: Vec<HlSpan> = all
                    .iter()
                    .copied()
                    .filter(|s| s.end_byte > line_start && s.start_byte < line_end)
                    .collect();
                spans.extend(styled_line(raw, line_start, &mine, syntax, ed.theme(), tab));
            }
            None => spans.push(Span::raw(expand_tabs(raw, tab))),
        }
        // Under the selection: a guide or a swatch has to let a selected line
        // still look selected.
        let line_start_char = buffer.rope().line_to_char(row);
        let line = Row { row, raw, start: line_start_char, gutter, tab };
        spans = decorate(spans, &line, &decorations, Layer::Under);

        // Search matches, under the selection so a selected match still reads
        // as selected. Bounded by the row, like every other pass here.
        if options.hlsearch
            && let Some(search) = &ed.session.last_search
        {
            let line_start = buffer.rope().line_to_char(row);
            let line_end = line_start + raw.chars().count();
            for (start, end) in
                buffer.matches_in(line_start, line_end, &search.pattern, search.whole_word)
            {
                let from = display_col(raw, start.saturating_sub(line_start), tab);
                let to = display_col(raw, (end - line_start).min(raw.chars().count()), tab);
                spans = paint_range(spans, (from + gutter)..(to + gutter), ed.theme().ui.search);
            }
        }

        // Selected columns on this row, in screen columns and offset past the
        // gutter. Charwise includes the character under the head; linewise
        // covers the row whatever the columns are.
        for selection in selections.all() {
            // A collapsed selection still covers something in visual mode — one
            // character for `v`, the whole line for `V` — so only skip it
            // outside visual, where it is a plain cursor.
            if selection.is_collapsed() && ed.session.mode.visual().is_none() {
                continue;
            }
            let (lo, hi) = selection.range();
            let (first, last) = (buffer.row_at(Cursor::at(lo)), buffer.row_at(Cursor::at(hi)));
            if row < first || row > last {
                continue;
            }
            let cols = match ed.session.mode.visual() {
                Some(VisualKind::Line) => 0..raw.chars().count().max(1),
                // A rectangle says nothing about char ranges, so the block
                // reads its own spans rather than the selection's range.
                Some(VisualKind::Block) => {
                    let line_start = buffer.rope().line_to_char(row);
                    let (start, end) = ed.block_span_in(id, row);
                    let (from, to) = (start - line_start, end - line_start);
                    display_col(raw, from, tab)
                        ..display_col(raw, to, tab).max(display_col(raw, from, tab) + 1)
                }
                _ => {
                    let line_start = buffer.rope().line_to_char(row);
                    let from = lo.saturating_sub(line_start).min(raw.chars().count());
                    let to = if row < last {
                        raw.chars().count()
                    } else {
                        (hi - line_start + 1).min(raw.chars().count())
                    };
                    display_col(raw, from, tab)
                        ..display_col(raw, to, tab).max(display_col(raw, from, tab) + 1)
                }
            };
            let cols = (cols.start + gutter)..(cols.end + gutter);
            spans = paint_range(spans, cols, ed.theme().ui.selection);
        }

        // The terminal's own cursor sits on the primary head; the others have
        // to be painted or they are invisible.
        if selections.len() > 1 {
            for (i, selection) in selections.all().iter().enumerate() {
                if i == selections.primary_index() {
                    continue;
                }
                let head = selection.head;
                if buffer.row_at(head) != row {
                    continue;
                }
                let col = display_col(raw, buffer.col_at(head), tab) + gutter;
                spans = paint_range(spans, col..col + 1, ed.theme().ui.cursor_alt);
            }
        }

        // Over everything: a letter you are about to press has to be readable
        // wherever it lands.
        spans = decorate(spans, &line, &decorations, Layer::Over);
        // And last of all the ones that make their own cells, because every
        // column above this point is a column of the text as it stands.
        spans = insert_inline(spans, &line, &decorations);

        // The cursor line is lit only in the focused window, as vim's
        // `'cursorline'` is. A dark bar in every pane reads as noise rather
        // than as a place.
        lines.push(if row == cursor_row && focused {
            fill_line(spans, ed.theme().ui.cursorline.bg, text_area.width as usize)
        } else {
            Line::from(spans)
        });
    }

    // Past-the-end rows, so an empty buffer doesn't look like a hang.
    while lines.len() < text_area.height as usize {
        lines.push(Line::from(Span::styled("~", tui(ed.theme().ui.filler))));
    }

    frame.render_widget(Paragraph::new(lines), text_area);

    focused.then(|| {
        (
            text_area.x + (gutter + cursor_screen_col) as u16,
            text_area.y + (cursor_row.saturating_sub(scroll)) as u16,
        )
    })
}

/// One window's own status row: what it is showing, and where in it.
///
/// Reverse-video when focused, dim when not. This is the focus indicator,
/// which is why the panes need no borders around them.
/// The colour a mode announces itself in. One table, wherever it is drawn.
fn mode_style(mode: &Mode, ui: &Ui) -> Style {
    tui(match mode {
        Mode::Insert => ui.mode_insert,
        // Both are overlays waiting for one key, so both read as the same
        // kind of moment.
        Mode::Pick | Mode::Label => ui.mode_pick,
        _ => ui.mode_normal,
    })
}

/// One window's status row.
///
/// The focused one ends with the mode, so what you are typing and where you
/// are typing it read as one line rather than two places on the screen. The
/// rest is reverse-video against dim, which is the focus indicator windows.md
/// picked instead of borders.
fn window_status(ed: &Editor, id: WindowId, focused: bool, width: u16) -> Vec<Span<'static>> {
    let row =
        if focused { tui(ed.theme().ui.statusline) } else { tui(ed.theme().ui.status_inactive) };
    let left = window_status_text(ed, id, focused);
    let right = if focused { format!(" {} ", ed.session.mode.label()) } else { String::new() };

    let pad = (width as usize).saturating_sub(left.chars().count() + right.chars().count());
    let mut spans = vec![Span::styled(left, row), Span::styled(" ".repeat(pad), row)];
    if focused {
        spans.push(Span::styled(right, mode_style(&ed.session.mode, &ed.theme().ui)));
    }
    spans
}

/// Name and modified marker at the left, `row:col` pushed to the right edge.
///
/// Split out for the same reason [`status_spans`] is: a `Paragraph` cannot be
/// read back, and what the row says is the thing worth guarding.
fn window_status_text(ed: &Editor, id: WindowId, focused: bool) -> String {
    // Text panes only: a tree has no status row at all, because its own first
    // row already names the root and a sidebar cannot spare a line to repeat
    // it. See `render`, which gives a tree pane its whole rect.
    let (name, at) = match ed.pane(id) {
        None | Some(Pane::Tree { .. }) => return String::new(),
        Some(Pane::Text { text, buffer, .. }) => {
            // The file name, not the path. Which `main.rs` it is belongs to the
            // picker; a pane thirty columns wide has no room to say it twice.
            let name = buffer
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "[No Name]".into());
            let cursor = text.selections.cursor();
            (
                format!("{name}{}", if buffer.is_modified() { " [+]" } else { "" }),
                format!("{}:{}", buffer.row_at(cursor) + 1, buffer.col_at(cursor) + 1),
            )
        }
    };

    // The focused pane leads with where the cursor is, because that is what you
    // want from the window you are typing in; the others lead with what they
    // are, because that is what you want from a window you are not.
    //
    // The modified marker rides with the name in both. It matters *most* on a
    // pane you are not looking at.
    match focused {
        true => format!(" {at}  {name}"),
        false => format!(" {name}  {at}"),
    }
}

/// The entry's first line, tab-expanded and elided to fit one row.
fn row_label(text: &str, width: usize, tab_width: usize) -> String {
    let first = text.lines().next().unwrap_or("");
    let expanded = expand_tabs(first, tab_width);
    if expanded.chars().count() <= width {
        return expanded;
    }
    expanded.chars().take(width.saturating_sub(1)).chain(std::iter::once('…')).collect()
}

/// A centred box over the buffer: query, match list, preview.
///
/// Viewport-bounded like the main pass — only visible rows are formatted, no
/// matter how deep the ring is.
fn render_picker(frame: &mut Frame, picker: &mut Picker, area: Rect, ui: &Ui, tab_width: usize) {
    let w = (area.width * 3 / 5).clamp(24, area.width.saturating_sub(2).max(24));
    let h = (area.height * 3 / 5).clamp(6, area.height.saturating_sub(2).max(6));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    };

    frame.render_widget(Clear, rect);
    // `Clear` resets cells to the terminal's own, which would punch a hole in
    // a theme that claimed the background.
    paint_base(frame, rect, ui);
    let outer = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(tui(ui.picker_border))
        .title(match picker.kind {
            PickerKind::Register { .. } => " registers ",
            PickerKind::Buffer => " buffers ",
            PickerKind::File => " files ",
            PickerKind::TreeRow => " tree ",
            PickerKind::History => " history ",
        });
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);
    if inner.height < 3 {
        return;
    }

    // A history row is a whole command line already, so previewing it would
    // show the same text twice and take a third of the overlay to do it.
    let preview_h = match picker.kind.wants_preview() {
        true => (inner.height / 3).max(1) + 1,
        false => 0,
    };
    let [query_area, list_area, preview_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(preview_h),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", tui(ui.picker_prompt)),
            Span::raw(picker.query().to_string()),
        ])),
        query_area,
    );

    picker.scroll_to_selected(list_area.height as usize);
    let width = list_area.width as usize;

    let rows: Vec<Line> = picker
        .matches()
        .iter()
        .enumerate()
        .skip(picker.scroll())
        .take(list_area.height as usize)
        .map(|(row, &i)| {
            let item = &picker.items()[i];
            let selected = row == picker.selected_row();
            let marker = if selected { "▸" } else { " " };
            // `¶` says the entry is linewise, so you can tell before pasting
            // whether it will open a line or splice inline.
            let badge = item.badge.unwrap_or(' ');
            let label = row_label(&item.text, width.saturating_sub(4), tab_width);
            let style = if selected { tui(ui.picker_selected) } else { Style::default() };
            Line::from(vec![
                Span::styled(format!("{marker} "), style),
                Span::styled(format!("{badge} "), tui(ui.picker_badge)),
                Span::styled(label, style),
            ])
        })
        .collect();

    let empty = rows.is_empty();
    frame.render_widget(Paragraph::new(rows), list_area);

    if preview_h == 0 {
        set_picker_cursor(frame, picker, query_area);
        return;
    }

    let preview = Block::default().borders(Borders::TOP).border_style(tui(ui.picker_divider));
    let preview_inner = preview.inner(preview_area);
    frame.render_widget(preview, preview_area);

    if !empty {
        let body: Vec<Line> = picker
            .preview()
            .lines()
            .take(preview_inner.height as usize)
            .map(|l| Line::from(expand_tabs(l, tab_width)))
            .collect();
        frame.render_widget(Paragraph::new(body).style(tui(ui.picker_preview)), preview_inner);
    }

    set_picker_cursor(frame, picker, query_area);
}

/// After the `> ` prompt, at the end of what has been typed.
fn set_picker_cursor(frame: &mut Frame, picker: &Picker, query_area: Rect) {
    frame.set_cursor_position((
        query_area.x + 2 + picker.query().chars().count() as u16,
        query_area.y,
    ));
}

fn status_line(
    ed: &Editor,
    pending: &str,
    matches: Option<(usize, usize)>,
    width: u16,
) -> Paragraph<'static> {
    Paragraph::new(Line::from(status_spans(ed, pending, matches, width)))
}

/// The footer, left to right: the message — then the pending keys and the mode,
/// pushed to the right edge.
///
/// The name, the modified marker and `row:col` used to live here. They belong
/// to a window rather than to the session, and now that there can be several
/// they are drawn on each window's own status row; repeating the focused one
/// down here would just be the same fact twice.
///
/// Unless the search has the line, in which case it is the pattern and the
/// match count and nothing else. `matches` is therefore only read there: a
/// remembered pattern is not a reason to keep counting at someone who has moved
/// on.
///
/// Split out from [`status_line`] because a `Paragraph` cannot be read back,
/// and the order is the thing worth guarding.
fn status_spans(
    ed: &Editor,
    pending: &str,
    matches: Option<(usize, usize)>,
    width: u16,
) -> Vec<Span<'static>> {
    if let Mode::Command(line) = &ed.session.mode {
        return vec![Span::raw(format!(":{line}"))];
    }
    if let Mode::Search { query, forward } = &ed.session.mode {
        let prefix = if *forward { '/' } else { '?' };
        return vec![Span::raw(format!("{prefix}{query}"))];
    }
    // The search keeps the line after `<CR>` too, for as long as the keys are
    // still the search. The pattern reads the same as it did while it was
    // being typed, which is the point: nothing else moves in or out around it.
    if ed.session.search_focus {
        let left = Span::raw(ed.session.status.clone());
        let right = match matches {
            Some((at, total)) => format!("[{at}/{total}] "),
            None => String::new(),
        };
        let pad =
            (width as usize).saturating_sub(left.content.chars().count() + right.chars().count());
        return vec![
            left,
            Span::raw(" ".repeat(pad)),
            Span::styled(right, tui(ed.theme().ui.status_muted)),
        ];
    }

    let mut spans =
        vec![Span::raw(" "), Span::styled(ed.session.status.clone(), tui(ed.theme().ui.status))];

    // Several cursors is a state you cannot otherwise tell from the mode: the
    // label still says NORMAL, and the only other sign is coloured cells that
    // may be scrolled off. Say how many.
    let cursors = ed.selections().map_or(0, Selections::len);
    let count = if cursors > 1 { format!("{cursors} cursors  ") } else { String::new() };
    let keys = format!("{count}{pending}  ");

    // The mode lives on the focused window's row now, beside the position it
    // applies to. What is left here is the session's: messages, half-typed
    // keys, and the `:` and `/` lines above.
    let left_width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = (width as usize).saturating_sub(left_width + keys.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(keys, tui(ed.theme().ui.status_muted)));

    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use bi::editor::LineNumbers;

    /// Any background will do for the padding tests below — they are about
    /// geometry, not colour.
    const CURSOR_LINE: ThemeColor = ThemeColor::Indexed(236);

    /// The colour table left this file — it is `bi::theme` now, and the
    /// fallback walk with it, because a GUI frontend needs the identical one.
    /// What stays here is the conversion, and all three spellings have to
    /// survive it.
    #[test]
    fn every_colour_spelling_survives_the_trip_to_ratatui() {
        assert_eq!(color(ThemeColor::Ansi(Ansi::Magenta)), Color::Magenta);
        assert_eq!(color(ThemeColor::Indexed(236)), Color::Indexed(236));
        assert_eq!(color(ThemeColor::Rgb(0xfb, 0x49, 0x34)), Color::Rgb(0xfb, 0x49, 0x34));

        let bold_italic = ThemeStyle {
            fg: Some(ThemeColor::Ansi(Ansi::Green)),
            bold: true,
            italic: true,
            ..ThemeStyle::default()
        };
        let converted = tui(bold_italic);
        assert_eq!(converted.fg, Some(Color::Green));
        assert!(converted.add_modifier.contains(Modifier::BOLD));
        assert!(converted.add_modifier.contains(Modifier::ITALIC));
    }

    /// A capture styled the colour of nothing is a capture that did not
    /// happen — `operator` sat on `Color::Gray`, which is what an unstyled
    /// cell already prints, so `&` in Go looked uncaptured. The default theme
    /// has to keep clearing that bar after the table moved.
    #[test]
    fn the_default_theme_draws_an_operator_as_something() {
        let theme = Theme::default();
        let operator = tui(theme.style("operator").expect("operator is themed"));
        assert_ne!(operator, Style::default());
        assert_ne!(operator, tui(theme.style("punctuation.bracket").unwrap()));
    }

    /// A theme that claims a background gets one painted; a theme that does
    /// not is invisible here, and the terminal's own shows through.
    #[test]
    fn only_a_theme_that_names_a_background_paints_one() {
        let gruvbox = Theme::default();
        assert_ne!(base_style(&gruvbox.ui), Style::default());
        assert_eq!(base_style(&gruvbox.ui).bg, Some(Color::Rgb(0x28, 0x28, 0x28)));

        let (ansi, problems) = Theme::resolve("ansi", None);
        assert_eq!(problems, []);
        assert_eq!(base_style(&ansi.ui), Style::default(), "ansi must not paint one");
    }

    #[test]
    fn the_footer_reads_the_message_and_no_longer_carries_the_mode() {
        let mut ed = Editor::empty();
        ed.session.status = "written".into();
        let footer: String =
            status_spans(&ed, "", None, 80).iter().map(|s| s.content.to_string()).collect();

        assert!(footer.contains("written"), "the message is the footer's job: {footer:?}");
        assert!(!footer.contains("NORMAL"), "the mode moved to the active window: {footer:?}");
    }

    /// The active pane leads with where the cursor is and ends with the mode.
    /// Its name is the file name — the path is what the picker is for, and a
    /// pane thirty columns wide has no room to repeat it.
    #[test]
    fn the_active_window_leads_with_the_position_and_ends_with_the_mode() {
        let mut ed = Editor::open("src/main.rs").unwrap();
        let row = window_status_text(&ed, ed.focus(), true);

        assert!(row.starts_with(" 1:1"), "position first: {row:?}");
        assert!(row.contains("main.rs"), "{row:?}");
        assert!(!row.contains('/'), "the name alone, not the path: {row:?}");

        let line = window_status(&ed, ed.focus(), true, 40);
        let text: String = line.iter().map(|s| s.content.to_string()).collect();
        assert!(text.ends_with(" NORMAL "), "and the mode at the right edge: {text:?}");
        assert_eq!(text.chars().count(), 40, "filling the pane");

        ed.buffer_mut().unwrap().insert_str(Cursor::at(0), "x");
        assert!(window_status_text(&ed, ed.focus(), true).contains("[+]"), "modified is marked");
    }

    /// An unfocused pane leads with what it *is*, because that is what you
    /// want from a window you are not typing in. No mode: there is one, and it
    /// belongs to the window that has the cursor.
    #[test]
    fn an_inactive_window_leads_with_its_name_and_carries_no_mode() {
        let ed = Editor::open("src/main.rs").unwrap();
        let row = window_status_text(&ed, ed.focus(), false);

        assert!(row.starts_with(" main.rs"), "name first: {row:?}");
        assert!(row.trim_end().ends_with("1:1"), "then the position: {row:?}");

        let line = window_status(&ed, ed.focus(), false, 40);
        let text: String = line.iter().map(|s| s.content.to_string()).collect();
        assert!(!text.contains("NORMAL"), "{text:?}");
    }

    /// The mark column is one character wide whether anything is marked or
    /// not, so marking a file does not shift the tree sideways under you.
    #[test]
    fn a_marked_row_is_flagged_without_moving_the_ones_around_it() {
        let row = TreeRow {
            path: "src/lib.rs".into(),
            name: "lib.rs".into(),
            depth: 1,
            kind: Kind::File,
            open: false,
        };

        let (plain, name) = tree_row_parts(&row, None);
        assert_eq!(plain, "     ", "mark column, one level of indent, no marker");
        assert_eq!(name, "lib.rs");

        let (copy, _) = tree_row_parts(&row, Some(ClipMode::Copy));
        let (cut, _) = tree_row_parts(&row, Some(ClipMode::Cut));
        assert_eq!(copy, "+    ");
        assert_eq!(cut, "~    ");
        assert_eq!(copy.chars().count(), plain.chars().count(), "same width either way");
    }

    /// A tree has no status row: its own first row already names the root, and
    /// a thirty-column sidebar cannot spare a line to say it twice.
    #[test]
    fn a_tree_pane_has_no_status_row() {
        let dir = std::env::temp_dir().join(format!("bi-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let ed = Editor::open(&dir).unwrap();

        assert_eq!(window_status_text(&ed, ed.focus(), true), "");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The footer describes the session; the window row describes the window.
    /// Repeating the focused window's name down there would be the same fact
    /// twice.
    #[test]
    fn the_footer_does_not_repeat_what_the_window_row_says() {
        let ed = Editor::empty();
        let footer: String =
            status_spans(&ed, "", None, 80).iter().map(|s| s.content.to_string()).collect();

        assert!(!footer.contains("[No Name]"), "{footer:?}");
        assert!(!footer.contains("1:1"), "{footer:?}");
    }

    #[test]
    fn the_gutter_keeps_its_width_in_every_mode_but_off() {
        let mut ed = Editor::empty();
        ed.buffer_mut().unwrap().insert_str(Cursor::at(0), &"x\n".repeat(120));
        let mut options = ed.session.options.clone();
        let numbered = gutter_width(&options, ed.buffer().unwrap());
        assert_eq!(numbered, 4, "121 lines, so three digits and a space");

        options.number = LineNumbers::Relative;
        assert_eq!(
            gutter_width(&options, ed.buffer().unwrap()),
            numbered,
            "or the file slides sideways as you move"
        );
        options.number = LineNumbers::Every(10);
        assert_eq!(gutter_width(&options, ed.buffer().unwrap()), numbered);

        options.number = LineNumbers::Off;
        assert_eq!(
            gutter_width(&options, ed.buffer().unwrap()),
            0,
            "the column is gone, not blank"
        );
    }

    /// The whole pipeline, on a real frame: core answers what to draw, this
    /// file draws it.
    ///
    /// The unit tests above pin the column arithmetic; this pins that the
    /// arithmetic is actually reached — a decoration produced and not painted
    /// looks exactly like a decoration never produced.
    #[test]
    fn indent_guides_reach_the_screen() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut ed = Editor::empty();
        ed.buffer_mut()
            .unwrap()
            .insert_str(Cursor::at(0), "fn main() {\n    let x = 1;\n        deep();\n}\n");
        ed.set_cursor(Cursor::at(0));

        let mut terminal = Terminal::new(TestBackend::new(24, 6)).unwrap();
        terminal.draw(|frame| render(frame, &mut ed, "")).unwrap();

        let rows: Vec<String> = (0..4)
            .map(|y| {
                (0..24)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();

        // The gutter comes too, because a guide's column is a column of the
        // *text*, and getting that offset wrong is the other thing this test
        // is here to catch.
        assert_eq!(rows, ["1 fn main() {", "2 │   let x = 1;", "3 │   │   deep();", "4 }",]);
    }

    /// The same pipeline for `Eol`, which needs a file on disk rather than a
    /// scratch buffer: no path, no grammar, and no block to name. See
    /// `docs/specs/tree-sitter-context.md`.
    #[test]
    fn the_context_annotation_reaches_the_screen() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        const SOURCE: &str = "int main(void) {\n    if (v == 0) {\n        go();\n    }\n}\n";
        let path = std::env::temp_dir().join(format!("bi-render-{}-ctx.c", std::process::id()));
        std::fs::write(&path, SOURCE).unwrap();
        let mut ed = Editor::open(path.to_str().unwrap()).unwrap();
        ed.set_cursor(Cursor::at(SOURCE.find("go()").unwrap()));

        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal.draw(|frame| render(frame, &mut ed, "")).unwrap();
        let _ = std::fs::remove_file(&path);

        let rows: Vec<String> = (0..5)
            .map(|y| {
                (0..40)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();

        assert_eq!(rows[3], "4 │   } // if (v == 0) {", "past the brace that closes it");
        assert_eq!(rows[2], "3 │   │   go();", "and no other row grew");
    }

    /// The other end of the same pipeline, for the decoration that moves a
    /// cell: the letter is on the screen *and* so is everything that was there
    /// before it.
    #[test]
    fn a_jump_label_reaches_the_screen_without_eating_the_text() {
        use bi::editor::{Action, Command};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut ed = Editor::empty();
        ed.buffer_mut().unwrap().insert_str(Cursor::at(0), "alpha beta gamma\n");
        ed.set_cursor(Cursor::at(0));

        let mut terminal = Terminal::new(TestBackend::new(24, 6)).unwrap();
        // Once first, so the window learns how tall it is: `s` aims at the
        // viewport, and a window of nought rows is showing nothing.
        terminal.draw(|frame| render(frame, &mut ed, "")).unwrap();

        ed.apply(Command { count: 1, action: Action::EnterFind });
        for c in "beta".chars() {
            ed.apply(Command { count: 1, action: Action::FindChar(c) });
        }
        terminal.draw(|frame| render(frame, &mut ed, "")).unwrap();

        let row: String = (0..24)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string();

        assert_eq!(row, "1 alpha betaf gamma", "the space after `beta` is still a space");
    }

    fn line_of(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.to_string()).collect()
    }

    /// The two anchors, on a line with a tab in it — the conversion from
    /// characters to columns is the part that can be wrong, and only a range
    /// anchored to text can be wrong that way.
    #[test]
    fn a_repaint_covers_the_columns_its_char_range_occupies() {
        let spans = vec![Span::raw(expand_tabs("\tab", 4))];
        let marked = ThemeStyle { bg: Some(ThemeColor::Indexed(1)), ..ThemeStyle::default() };
        // Chars 1..2 — the `a`, which a tab has pushed out to column 4.
        let line = Row { row: 0, raw: "\tab", start: 0, gutter: 0, tab: 4 };
        let decorations = [Decoration::Repaint { range: 1..2, style: marked, layer: Layer::Under }];

        let out = decorate(spans, &line, &decorations, Layer::Under);

        assert_eq!(line_of(&out), "    ab", "the text is untouched");
        let painted: String = out
            .iter()
            .filter(|s| s.style.bg == Some(Color::Indexed(1)))
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(painted, "a", "and exactly one column changed colour");
    }

    #[test]
    fn an_eol_decoration_lands_past_the_end_and_pads_nothing() {
        let line = Row { row: 0, raw: "code", start: 0, gutter: 0, tab: 4 };
        let decorations = [Decoration::Eol {
            row: 0,
            text: "  ← here".to_string(),
            style: ThemeStyle::default(),
        }];

        let out = decorate(vec![Span::raw("code".to_string())], &line, &decorations, Layer::Over);

        assert_eq!(line_of(&out), "code  ← here");
    }

    /// The whole point of `Inline`: the character the label points at is still
    /// on the screen, one cell further along.
    #[test]
    fn an_inline_label_drops_nothing() {
        let line = Row { row: 0, raw: "\tcode", start: 0, gutter: 2, tab: 4 };
        let spans = vec![Span::raw("  ".to_string()), Span::raw(expand_tabs("\tcode", 4))];
        let decorations = [Decoration::Inline {
            row: 0,
            col: 4,
            text: "f".to_string(),
            style: ThemeStyle::default(),
        }];

        let out = insert_inline(spans, &line, &decorations);

        // Column 4 is the `c`, which a tab has pushed out there, and the
        // gutter is the frontend's own two cells in front of all of it.
        assert_eq!(line_of(&out), "      fcode");
    }

    /// Two labels wanting one column are two cells side by side, in the order
    /// they were produced — which is what `S` needs to say where a scope ends
    /// when another one ends in the same place.
    #[test]
    fn two_inline_labels_at_one_column_are_two_cells() {
        let line = Row { row: 0, raw: "ab", start: 0, gutter: 0, tab: 4 };
        let at = |col: usize, text: &str| Decoration::Inline {
            row: 0,
            col,
            text: text.to_string(),
            style: ThemeStyle::default(),
        };
        // Out of order on purpose: the columns name the row as it stands, so
        // the later one must not have moved by the time it is placed.
        let decorations = [at(1, "y"), at(0, "x"), at(1, "z")];

        let out = insert_inline(vec![Span::raw("ab".to_string())], &line, &decorations);

        assert_eq!(line_of(&out), "xayzb");
    }

    /// The one thing the inline pass has to tell the rest of the frame: where
    /// the cursor went. A label at the cursor's own column goes in front of
    /// it, because it is pointing at the character the cursor is on.
    #[test]
    fn the_cursor_moves_over_by_the_labels_in_front_of_it() {
        let at = |col: usize| Decoration::Inline {
            row: 0,
            col,
            text: "ab".to_string(),
            style: ThemeStyle::default(),
        };
        let decorations = [at(0), at(4), at(9)];

        assert_eq!(inline_shift(&decorations, 0, 4), 4, "the two at or before column 4");
        assert_eq!(inline_shift(&decorations, 0, 0), 2);
        assert_eq!(inline_shift(&decorations, 1, 9), 0, "and nothing from another row");
    }

    /// The whole of what `Layer` is for: a guide has to let a selected line
    /// still look selected, and a jump label has to be readable wherever it
    /// lands.
    #[test]
    fn the_selection_paints_over_one_layer_and_under_the_other() {
        let line = Row { row: 0, raw: "code", start: 0, gutter: 0, tab: 4 };
        let ink = ThemeStyle { bg: Some(ThemeColor::Indexed(1)), ..ThemeStyle::default() };
        let selection = ThemeStyle { bg: Some(ThemeColor::Indexed(2)), ..ThemeStyle::default() };
        let under = [Decoration::Overlay {
            row: 0,
            col: 0,
            text: "│".to_string(),
            style: ink,
            layer: Layer::Under,
        }];
        let over = [Decoration::Overlay {
            row: 0,
            col: 3,
            text: "x".to_string(),
            style: ink,
            layer: Layer::Over,
        }];

        // The order render_window paints in: under, selection, over.
        let mut spans = vec![Span::raw("code".to_string())];
        spans = decorate(spans, &line, &under, Layer::Under);
        spans = paint_range(spans, 0..4, selection);
        spans = decorate(spans, &line, &over, Layer::Over);

        assert_eq!(line_of(&spans), "│odx");
        let bg = |col: usize| {
            spans
                .iter()
                .flat_map(|s| std::iter::repeat_n(s.style.bg, s.content.chars().count()))
                .nth(col)
                .flatten()
        };
        assert_eq!(bg(0), Some(Color::Indexed(2)), "the selection is over the guide");
        assert_eq!(bg(3), Some(Color::Indexed(1)), "and under the label");
    }

    /// The whole of what a frontend does with a decoration, on one line.
    ///
    /// Width is the property worth guarding: an overlay replaces the cells it
    /// covers rather than pushing them right, so the line it comes out of is
    /// exactly as long as the line that went in — otherwise every column past
    /// it, the cursor included, would be somewhere else.
    #[test]
    fn an_overlay_replaces_cells_rather_than_pushing_them() {
        let spans = vec![Span::raw("    code".to_string())];
        let width: usize = spans.iter().map(|s| s.content.chars().count()).sum();

        let out = overlay(spans, 4, "│", ThemeStyle::default());

        let text: String = out.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "    │ode");
        assert_eq!(text.chars().count(), width, "the same length it went in");
    }

    /// A guide on a blank line is past the end of it, which is a line that has
    /// to grow a little to hold one.
    #[test]
    fn an_overlay_past_the_end_of_a_line_pads_up_to_it() {
        let out = overlay(vec![Span::raw(String::new())], 4, "│", ThemeStyle::default());
        let text: String = out.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "    │");
    }

    /// The case a char offset could not have expressed: column 4 of a
    /// tab-indented line is *inside* the tab.
    #[test]
    fn an_overlay_lands_inside_a_tab_expansion() {
        // One tab at width 8, so the line is eight columns of nothing.
        let spans = vec![Span::raw(expand_tabs("\tcode", 8))];
        let out = overlay(spans, 4, "│", ThemeStyle::default());
        let text: String = out.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "    │   code");
    }

    #[test]
    fn split_at_col_keeps_both_halves_and_their_styles() {
        let red = Style::default().fg(Color::Red);
        let spans = vec![Span::styled("abcd".to_string(), red)];
        let (left, right) = split_at_col(spans, 2);
        assert_eq!(left.iter().map(|s| s.content.to_string()).collect::<String>(), "ab");
        assert_eq!(right.iter().map(|s| s.content.to_string()).collect::<String>(), "cd");
        assert_eq!(left[0].style, red, "and neither half loses its colour");
        assert_eq!(right[0].style, red);
    }

    /// The renderer holds no width of its own any more — every column it
    /// counts comes from `options.tab_width`, through `bi::indent`. `row_label`
    /// is the smallest place that shows it: the same text, two widths.
    #[test]
    fn a_tab_is_as_wide_as_the_options_say() {
        assert_eq!(row_label("a\tb", 20, 4), "a   b");
        assert_eq!(row_label("a\tb", 20, 8), "a       b");
    }

    #[test]
    fn a_search_takes_the_whole_footer_and_puts_the_count_on_the_right() {
        let mut ed = Editor::empty();
        ed.session.search_focus = true;
        ed.session.status = "/foo".into();

        let spans = status_spans(&ed, "", Some((3, 17)), 20);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "/foo         [3/17] ", "the pattern, the count, nothing else");
    }

    #[test]
    fn the_count_goes_away_with_the_search_even_though_the_pattern_is_remembered() {
        let ed = Editor::empty();
        let text: String =
            status_spans(&ed, "", Some((3, 17)), 80).iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("[3/17]"), "a remembered pattern is not a live search");
    }

    #[test]
    fn the_footer_fills_the_width_exactly() {
        let ed = Editor::empty();
        let width: usize =
            status_spans(&ed, "d3", None, 80).iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 80, "or the mode block would not touch the right edge");
    }

    #[test]
    fn the_cursor_line_is_padded_to_the_full_width() {
        let line = fill_line(vec![Span::raw("abc")], Some(CURSOR_LINE), 10);
        let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 10, "highlight must reach the edge of the pane");
        assert!(line.spans.iter().all(|s| s.style.bg == Some(color(CURSOR_LINE))));
    }

    #[test]
    fn the_background_does_not_disturb_syntax_colours() {
        let spans =
            vec![Span::styled("kw", Style::default().fg(Color::Magenta)), Span::raw(" plain")];
        let line = fill_line(spans, Some(CURSOR_LINE), 20);
        assert_eq!(line.spans[0].style.fg, Some(Color::Magenta));
        assert_eq!(line.spans[0].style.bg, Some(color(CURSOR_LINE)));
        assert_eq!(line.spans[1].style.fg, None);
    }

    #[test]
    fn a_line_wider_than_the_pane_is_not_truncated_or_panicked_on() {
        let line = fill_line(vec![Span::raw("a".repeat(30))], Some(CURSOR_LINE), 10);
        let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 30);
    }
}
