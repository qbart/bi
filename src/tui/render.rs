//! Rendering.
//!
//! Only the visible viewport is ever touched — the cost of a frame is bounded
//! by terminal height, not buffer size. Keep it that way.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use bee::editor::{Editor, Mode};
use bee::picker::Picker;
use bee::syntax::{Span as HlSpan, Syntax};

const TAB_WIDTH: usize = 4;

/// Background for the line the cursor is on.
///
/// Dark, because the rest of this table already assumes a dark terminal
/// (comments are `DarkGray`). It has to stay subtle: it sits behind syntax
/// colours rather than replacing them, so anything strong makes `comment`
/// unreadable. Goes wherever the highlight table goes when a theme file exists.
const CURSOR_LINE_BG: Color = Color::Indexed(236);

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
            let style = s.style.bg(bg);
            s.style(style)
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

pub fn render(frame: &mut Frame, ed: &mut Editor, pending: &str) {
    let [text_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());

    ed.scroll_to_cursor(text_area.height as usize);

    let total = ed.buffer.line_count();
    let gutter = format!("{total}").len() + 1;
    let cursor = ed.selections.cursor();
    let cursor_row = ed.buffer.row_at(cursor);

    let mut lines = Vec::with_capacity(text_area.height as usize);
    let mut cursor_screen_col = 0;

    // One query for the whole visible range, then partition per line. Bounded
    // by terminal height, never by file size.
    let last_row = (ed.scroll + text_area.height as usize).min(total);
    let highlights = ed.syntax.as_ref().map(|syntax| {
        let rope = ed.buffer.rope();
        let from = rope.line_to_byte(ed.scroll.min(rope.len_lines()));
        let to = rope.line_to_byte(last_row.min(rope.len_lines()));
        (syntax, syntax.highlights(rope, from..to))
    });

    for row in ed.scroll..last_row {
        let raw = ed.buffer.rope().line(row).to_string();
        let raw = raw.trim_end_matches(['\n', '\r']);

        if row == cursor_row {
            cursor_screen_col = display_col(raw, ed.buffer.col_at(cursor));
        }

        let number = Span::styled(
            format!("{:>width$} ", row + 1, width = gutter - 1),
            Style::default().fg(if row == cursor_row { Color::Yellow } else { Color::DarkGray }),
        );
        let mut spans = vec![number];
        match &highlights {
            Some((syntax, all)) => {
                let line_start = ed.buffer.rope().line_to_byte(row);
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
        lines.push(if row == cursor_row {
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
    frame.render_widget(status_line(ed, pending, status_area.width), status_area);

    if matches!(ed.mode, Mode::Pick)
        && let Some(picker) = ed.picker.as_mut()
    {
        render_picker(frame, picker, text_area);
        return;
    }

    match &ed.mode {
        Mode::Command(line) => {
            frame.set_cursor_position((
                status_area.x + 1 + line.chars().count() as u16,
                status_area.y,
            ));
        }
        _ => {
            frame.set_cursor_position((
                text_area.x + (gutter + cursor_screen_col) as u16,
                text_area.y + (cursor_row - ed.scroll) as u16,
            ));
        }
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
        .title(" registers ");
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

fn status_line(ed: &Editor, pending: &str, width: u16) -> Paragraph<'static> {
    if let Mode::Command(line) = &ed.mode {
        return Paragraph::new(Line::from(format!(":{line}")));
    }

    let name = ed
        .buffer
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "[No Name]".into());

    let mode_style = Style::default()
        .fg(Color::Black)
        .bg(match ed.mode {
            Mode::Insert => Color::Green,
            Mode::Pick => Color::Magenta,
            _ => Color::Blue,
        })
        .add_modifier(Modifier::BOLD);

    let mut spans = vec![
        Span::styled(format!(" {} ", ed.mode.label()), mode_style),
        Span::raw(format!(" {name}")),
        Span::styled(
            if ed.buffer.is_modified() { " [+]" } else { "" },
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  "),
        Span::styled(ed.status.clone(), Style::default().fg(Color::Cyan)),
    ];

    let right = format!(
        "{}  {}:{} ",
        pending,
        ed.buffer.row_at(ed.selections.cursor()) + 1,
        ed.buffer.col_at(ed.selections.cursor()) + 1
    );

    let left_width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = (width as usize).saturating_sub(left_width + right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, Style::default().fg(Color::DarkGray)));

    Paragraph::new(Line::from(spans))
}

#[cfg(test)]
mod tests {
    use super::*;

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
