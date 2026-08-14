//! Rendering.
//!
//! Only the visible viewport is ever touched — the cost of a frame is bounded
//! by terminal height, not buffer size. Keep it that way.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::editor::{Editor, Mode};

const TAB_WIDTH: usize = 4;

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

pub fn render(frame: &mut Frame, ed: &mut Editor, pending: &str) {
    let [text_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());

    ed.scroll_to_cursor(text_area.height as usize);

    let total = ed.buffer.line_count();
    let gutter = format!("{total}").len() + 1;
    let cursor_row = ed.buffer.cursor_row();

    let mut lines = Vec::with_capacity(text_area.height as usize);
    let mut cursor_screen_col = 0;

    for row in ed.scroll..(ed.scroll + text_area.height as usize).min(total) {
        let raw = ed.buffer.rope().line(row).to_string();
        let raw = raw.trim_end_matches(['\n', '\r']);

        if row == cursor_row {
            cursor_screen_col = display_col(raw, ed.buffer.cursor_col());
        }

        let number = Span::styled(
            format!("{:>width$} ", row + 1, width = gutter - 1),
            Style::default().fg(if row == cursor_row {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        );
        lines.push(Line::from(vec![number, Span::raw(expand_tabs(raw))]));
    }

    // Past-the-end rows, so an empty buffer doesn't look like a hang.
    while lines.len() < text_area.height as usize {
        lines.push(Line::from(Span::styled(
            "~",
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(Paragraph::new(lines), text_area);
    frame.render_widget(status_line(ed, pending, status_area.width), status_area);

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
        ed.buffer.cursor_row() + 1,
        ed.buffer.cursor_col() + 1
    );

    let left_width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = (width as usize).saturating_sub(left_width + right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, Style::default().fg(Color::DarkGray)));

    Paragraph::new(Line::from(spans))
}
