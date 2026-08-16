//! Rendering.
//!
//! Only the visible viewport is ever touched — the cost of a frame is bounded
//! by terminal height, not buffer size. Keep it that way.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use bee::buffer::Cursor;
use bee::editor::{Editor, LineNumbers, Mode, Pane, VisualKind};
use bee::picker::{Picker, PickerKind};
use bee::selection::Selections;
use bee::syntax::{Span as HlSpan, Syntax};
use bee::tree::{ClipMode, Clipboard, Kind, Row as TreeRow, Tree};
use bee::window::{Chrome, ContentKind, Rect as CoreRect, WindowId};

const TAB_WIDTH: usize = 4;

/// Background for the line the cursor is on.
///
/// Dark, because the rest of this table already assumes a dark terminal
/// (comments are `DarkGray`). It has to stay subtle: it sits behind syntax
/// colours rather than replacing them, so anything strong makes `comment`
/// unreadable. Goes wherever the highlight table goes when a theme file exists.
const CURSOR_LINE_BG: Color = Color::Indexed(236);

/// Background for selected text. Lighter than the cursor line, so a selection
/// that covers the cursor's line still reads as a selection.
const SELECTION_BG: Color = Color::Indexed(239);

/// A terminal has exactly one real cursor, and the primary selection gets it.
/// Every other head is drawn as a reversed cell instead.
const EXTRA_CURSOR_BG: Color = Color::Magenta;

/// Background for search matches. Distinct from the selection, since a match
/// can sit inside one.
const SEARCH_BG: Color = Color::Indexed(58);

/// Repaints the background of the columns in `cols` within an already-built
/// line, leaving the foreground alone.
///
/// Spans are split at the range's edges rather than styled whole, because a
/// selection almost never lines up with a syntax span.
fn paint_range(
    spans: Vec<Span<'static>>,
    cols: std::ops::Range<usize>,
    bg: Color,
) -> Vec<Span<'static>> {
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
        out.push(Span::styled(take(lo, hi), span.style.bg(bg)));
        if hi < width {
            out.push(Span::styled(take(hi, width), span.style));
        }
    }
    out
}

/// Paints `bg` behind a line and pads it to `width`, so the highlight reaches
/// the edge of the pane instead of stopping at the last character.
///
/// Span styles are patched rather than replaced — the syntax colours are the
/// foreground and have to survive.
fn fill_line(spans: Vec<Span<'static>>, bg: Color, width: usize) -> Line<'static> {
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

/// Expands tabs for display.
///
/// Width is counted in chars, so wide (CJK) and combining chars will be off.
/// Fixing that means a `unicode-width` dependency and a real grapheme walk —
/// worth doing before this is usable on non-Latin text.
fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let n = TAB_WIDTH - (col % TAB_WIDTH);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// Screen column of char offset `char_col` within `line`.
fn display_col(line: &str, char_col: usize) -> usize {
    let mut col = 0;
    for ch in line.chars().take(char_col) {
        if ch == '\t' {
            col += TAB_WIDTH - (col % TAB_WIDTH);
        } else {
            col += 1;
        }
    }
    col
}

/// Capture name to colour.
///
/// Matches on the part before the first dot, so `function.method` falls back to
/// `function` without needing an arm of its own. This table is the whole reason
/// `syntax.rs` emits names rather than styles: a GUI frontend writes its own,
/// and a theme file eventually replaces it.
fn style_for(name: &str) -> Style {
    let base = name.split('.').next().unwrap_or(name);
    match base {
        "keyword" => Style::default().fg(Color::Magenta),
        "function" | "constructor" => Style::default().fg(Color::Blue),
        "type" => Style::default().fg(Color::Cyan),
        "string" | "escape" | "character" => Style::default().fg(Color::Green),
        "comment" => Style::default().fg(Color::DarkGray),
        "constant" | "number" | "float" | "boolean" => Style::default().fg(Color::Yellow),
        "attribute" | "label" => Style::default().fg(Color::LightMagenta),
        "operator" | "punctuation" => Style::default().fg(Color::Gray),
        _ => Style::default(),
    }
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
                let n = TAB_WIDTH - (*col % TAB_WIDTH);
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
        push(&raw[start..end], style_for(syntax.capture_name(span.capture)), &mut col);
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
fn gutter_width(ed: &Editor, buffer: &bee::buffer::Buffer) -> usize {
    match ed.session.line_numbers {
        LineNumbers::Off => 0,
        _ => format!("{}", buffer.line_count()).len() + 1,
    }
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
                .map(|_| Line::from(Span::styled("│", Style::default().fg(Color::DarkGray))))
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
        render_picker(frame, picker, body);
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

/// Colour for a directory row. Blue is what `ls` has used for decades, and the
/// fingers know it before the eyes do.
const TREE_DIR: Color = Color::Blue;

/// A symlink. Cyan, again following `ls`.
const TREE_LINK: Color = Color::Cyan;

/// Marked to copy, and marked to cut. Yellow says "noted"; red says "this one
/// is leaving".
const MARK_COPY: Color = Color::Yellow;
const MARK_CUT: Color = Color::Red;

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
) -> Option<(u16, u16)> {
    let rows = tree.rows();
    let last = (tree.scroll() + area.height as usize).min(rows.len());
    let mut cursor_at = None;
    let mut lines = Vec::with_capacity(area.height as usize);

    for (index, row) in rows.iter().enumerate().take(last).skip(tree.scroll()) {
        let mark = clipboard.contains(&row.path).then(|| clipboard.mode());
        let (indent, name) = tree_row_parts(row, mark);

        let style = match row.kind {
            Kind::Dir => Style::default().fg(TREE_DIR).add_modifier(Modifier::BOLD),
            Kind::Link => Style::default().fg(TREE_LINK),
            Kind::File => Style::default(),
        };
        let mark_style = match mark {
            Some(ClipMode::Copy) => Style::default().fg(MARK_COPY).add_modifier(Modifier::BOLD),
            Some(ClipMode::Cut) => Style::default().fg(MARK_CUT).add_modifier(Modifier::BOLD),
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
        let bg = if focused { SELECTION_BG } else { CURSOR_LINE_BG };
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
    let (text, buffer, syntax) = match ed.pane(id)? {
        Pane::Text { text, buffer, syntax, .. } => (text, buffer, syntax),
        Pane::Tree { tree, .. } => {
            return render_tree(frame, tree, &ed.session.clipboard, text_area, focused);
        }
    };
    let (scroll, selections) = (text.scroll, &text.selections);

    let total = buffer.line_count();
    let gutter = gutter_width(ed, buffer);
    let cursor = selections.cursor();
    let cursor_row = buffer.row_at(cursor);

    let mut lines = Vec::with_capacity(text_area.height as usize);
    let mut cursor_screen_col = 0;

    // One query for the whole visible range, then partition per line. Bounded
    // by pane height, never by file size.
    let last_row = (scroll + text_area.height as usize).min(total);
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
            cursor_screen_col = display_col(raw, buffer.col_at(cursor));
        }

        // A blank cell where a number is not due, so the text stays put.
        let mut spans = match ed.session.line_numbers.label_for(row, cursor_row) {
            _ if gutter == 0 => Vec::new(),
            Some(n) => vec![Span::styled(
                format!("{n:>width$} ", width = gutter - 1),
                Style::default().fg(if row == cursor_row {
                    Color::Yellow
                } else {
                    Color::DarkGray
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
                spans.extend(styled_line(raw, line_start, &mine, syntax));
            }
            None => spans.push(Span::raw(expand_tabs(raw))),
        }
        // Search matches, under the selection so a selected match still reads
        // as selected. Bounded by the row, like every other pass here.
        if ed.session.highlight_search
            && let Some(search) = &ed.session.last_search
        {
            let line_start = buffer.rope().line_to_char(row);
            let line_end = line_start + raw.chars().count();
            for (start, end) in
                buffer.matches_in(line_start, line_end, &search.pattern, search.whole_word)
            {
                let from = display_col(raw, start.saturating_sub(line_start));
                let to = display_col(raw, (end - line_start).min(raw.chars().count()));
                spans = paint_range(spans, (from + gutter)..(to + gutter), SEARCH_BG);
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
                    display_col(raw, from)..display_col(raw, to).max(display_col(raw, from) + 1)
                }
                _ => {
                    let line_start = buffer.rope().line_to_char(row);
                    let from = lo.saturating_sub(line_start).min(raw.chars().count());
                    let to = if row < last {
                        raw.chars().count()
                    } else {
                        (hi - line_start + 1).min(raw.chars().count())
                    };
                    display_col(raw, from)..display_col(raw, to).max(display_col(raw, from) + 1)
                }
            };
            let cols = (cols.start + gutter)..(cols.end + gutter);
            spans = paint_range(spans, cols, SELECTION_BG);
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
                let col = display_col(raw, buffer.col_at(head)) + gutter;
                spans = paint_range(spans, col..col + 1, EXTRA_CURSOR_BG);
            }
        }

        // The cursor line is lit only in the focused window, as vim's
        // `'cursorline'` is. A dark bar in every pane reads as noise rather
        // than as a place.
        lines.push(if row == cursor_row && focused {
            fill_line(spans, CURSOR_LINE_BG, text_area.width as usize)
        } else {
            Line::from(spans)
        });
    }

    // Past-the-end rows, so an empty buffer doesn't look like a hang.
    while lines.len() < text_area.height as usize {
        lines.push(Line::from(Span::styled("~", Style::default().fg(Color::DarkGray))));
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
fn mode_style(mode: &Mode) -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(match mode {
            Mode::Insert => Color::Green,
            Mode::Pick => Color::Magenta,
            _ => Color::Blue,
        })
        .add_modifier(Modifier::BOLD)
}

/// One window's status row.
///
/// The focused one ends with the mode, so what you are typing and where you
/// are typing it read as one line rather than two places on the screen. The
/// rest is reverse-video against dim, which is the focus indicator windows.md
/// picked instead of borders.
fn window_status(ed: &Editor, id: WindowId, focused: bool, width: u16) -> Vec<Span<'static>> {
    let style = if focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let left = window_status_text(ed, id, focused);
    let right = if focused { format!(" {} ", ed.session.mode.label()) } else { String::new() };

    let pad = (width as usize).saturating_sub(left.chars().count() + right.chars().count());
    let mut spans = vec![Span::styled(left, style), Span::styled(" ".repeat(pad), style)];
    if focused {
        spans.push(Span::styled(right, mode_style(&ed.session.mode)));
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
fn row_label(text: &str, width: usize) -> String {
    let first = text.lines().next().unwrap_or("");
    let expanded = expand_tabs(first);
    if expanded.chars().count() <= width {
        return expanded;
    }
    expanded.chars().take(width.saturating_sub(1)).chain(std::iter::once('…')).collect()
}

/// A centred box over the buffer: query, match list, preview.
///
/// Viewport-bounded like the main pass — only visible rows are formatted, no
/// matter how deep the ring is.
fn render_picker(frame: &mut Frame, picker: &mut Picker, area: Rect) {
    let w = (area.width * 3 / 5).clamp(24, area.width.saturating_sub(2).max(24));
    let h = (area.height * 3 / 5).clamp(6, area.height.saturating_sub(2).max(6));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    };

    frame.render_widget(Clear, rect);
    let outer = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Magenta))
        .title(match picker.kind {
            PickerKind::Register { .. } => " registers ",
            PickerKind::Buffer => " buffers ",
        });
    let inner = outer.inner(rect);
    frame.render_widget(outer, rect);
    if inner.height < 3 {
        return;
    }

    let preview_h = (inner.height / 3).max(1) + 1;
    let [query_area, list_area, preview_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(preview_h),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Magenta)),
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
            let label = row_label(&item.text, width.saturating_sub(4));
            let style = if selected {
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(format!("{marker} "), style),
                Span::styled(format!("{badge} "), Style::default().fg(Color::DarkGray)),
                Span::styled(label, style),
            ])
        })
        .collect();

    let empty = rows.is_empty();
    frame.render_widget(Paragraph::new(rows), list_area);

    let preview =
        Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray));
    let preview_inner = preview.inner(preview_area);
    frame.render_widget(preview, preview_area);

    if !empty {
        let body: Vec<Line> = picker
            .preview()
            .lines()
            .take(preview_inner.height as usize)
            .map(|l| Line::from(expand_tabs(l)))
            .collect();
        frame.render_widget(
            Paragraph::new(body).style(Style::default().fg(Color::Gray)),
            preview_inner,
        );
    }

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
            Span::styled(right, Style::default().fg(Color::DarkGray)),
        ];
    }

    let mut spans = vec![
        Span::raw(" "),
        Span::styled(ed.session.status.clone(), Style::default().fg(Color::Cyan)),
    ];

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
    spans.push(Span::styled(keys, Style::default().fg(Color::DarkGray)));

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let dir = std::env::temp_dir().join(format!("bee-status-{}", std::process::id()));
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
        let numbered = gutter_width(&ed, ed.buffer().unwrap());
        assert_eq!(numbered, 4, "121 lines, so three digits and a space");

        ed.session.line_numbers = LineNumbers::Relative;
        assert_eq!(
            gutter_width(&ed, ed.buffer().unwrap()),
            numbered,
            "or the file slides sideways as you move"
        );
        ed.session.line_numbers = LineNumbers::Every(10);
        assert_eq!(gutter_width(&ed, ed.buffer().unwrap()), numbered);

        ed.session.line_numbers = LineNumbers::Off;
        assert_eq!(gutter_width(&ed, ed.buffer().unwrap()), 0, "the column is gone, not blank");
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
        let line = fill_line(vec![Span::raw("abc")], CURSOR_LINE_BG, 10);
        let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 10, "highlight must reach the edge of the pane");
        assert!(line.spans.iter().all(|s| s.style.bg == Some(CURSOR_LINE_BG)));
    }

    #[test]
    fn the_background_does_not_disturb_syntax_colours() {
        let spans =
            vec![Span::styled("kw", Style::default().fg(Color::Magenta)), Span::raw(" plain")];
        let line = fill_line(spans, CURSOR_LINE_BG, 20);
        assert_eq!(line.spans[0].style.fg, Some(Color::Magenta));
        assert_eq!(line.spans[0].style.bg, Some(CURSOR_LINE_BG));
        assert_eq!(line.spans[1].style.fg, None);
    }

    #[test]
    fn a_line_wider_than_the_pane_is_not_truncated_or_panicked_on() {
        let line = fill_line(vec![Span::raw("a".repeat(30))], CURSOR_LINE_BG, 10);
        let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, 30);
    }
}
