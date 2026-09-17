//! The window: one pane, the focused buffer, two status rows, and keys.
//!
//! Does what `tui::render::render` does, in the same order — cell size, layout,
//! text, status rows — with gpui's `Render` in place of ratatui's `draw`. See
//! `docs/specs/gui.md`.

use gpui::{
    AnyElement, Context, FocusHandle, FontStyle, FontWeight, Hsla, IntoElement, KeyDownEvent,
    Render, StyledText, Task, TextRun, UnderlineStyle, Window, div, font, prelude::*, px, rgb,
};

use bi::editor::{Editor, Mode, Pane};
use bi::indent::{display_col, glyph};
use bi::input::Input;
use bi::syntax::{Span as HlSpan, Syntax};
use bi::theme::{Color as ThemeColor, Style as ThemeStyle, Theme};
use bi::window::{Chrome, Rect};

/// Monospace families, in order of preference. gpui matches a family name
/// against what fontconfig reports and nothing else — there is no
/// `monospace` alias to ask for — so the first one present is the font.
const FAMILIES: &[&str] =
    &["DejaVu Sans Mono", "Liberation Mono", "Noto Mono", "Menlo", "Consolas", "Courier New"];
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;

/// What a theme that names no colours gets: the terminal's usual dark.
const DEFAULT_BG: u32 = 0x1e1e1e;
const DEFAULT_FG: u32 = 0xd4d4d4;

/// The terminal's chrome, until this frontend has reasons of its own.
const CHROME: Chrome = Chrome { columns: 1, rows: 0, min_width: 8, min_height: 2, tree_width: 30 };

pub struct View {
    editor: Editor,
    input: Input,
    /// Which `config_epoch` the keymap on `input` came from — see `run` in
    /// the terminal's `main.rs`, which does the same.
    installed: Option<u64>,
    focus: FocusHandle,
    family: &'static str,
    /// The pending redraw, for a flash that will expire or a checktime poll
    /// that will come due. Replaced every frame; dropping a task cancels it.
    timer: Option<Task<()>>,
}

impl View {
    pub fn new(editor: Editor, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let names = window.text_system().all_font_names();
        let family =
            FAMILIES.iter().copied().find(|f| names.iter().any(|n| n == f)).unwrap_or(FAMILIES[0]);
        Self { editor, input: Input::default(), installed: None, focus, family, timer: None }
    }

    /// The terminal loop's key arm: translate, resolve, apply, settle, draw.
    fn key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(key) = keys(ev) else { return };
        let ed = &mut self.editor;
        if let Some(cmd) = self.input.on_key(key, &ed.session.mode, ed.content_kind()) {
            ed.session.status.clear();
            ed.apply(cmd);
        }
        ed.settle();
        if ed.session.quit {
            cx.quit();
            return;
        }
        cx.notify();
    }

    fn run(&self, len: usize, look: Look) -> TextRun {
        let mut font = font(self.family);
        if look.bold {
            font.weight = FontWeight::BOLD;
        }
        if look.italic {
            font.style = FontStyle::Italic;
        }
        TextRun {
            len,
            font,
            color: look.fg,
            background_color: look.bg,
            underline: look.underline.then(|| UnderlineStyle {
                thickness: px(1.0),
                color: Some(look.fg),
                wavy: false,
            }),
            strikethrough: None,
        }
    }

    /// One row of cells as runs, with the cursor drawn in reverse video over
    /// the cell at `cursor`, if any. Past the end of the text the cursor sits
    /// on a blank cell, which is where insert mode puts it. Adjacent cells
    /// that look the same become one run: a run per cell would shape every
    /// glyph on its own.
    fn row(&self, cells: &[Cell], cursor: Option<usize>, base: Look, bg: Hsla) -> StyledText {
        let mut text = String::new();
        let mut runs: Vec<TextRun> = Vec::new();
        let mut last: Option<Look> = None;
        let blank = Cell { ch: ' ', look: base };
        let count = match cursor {
            Some(at) => cells.len().max(at + 1),
            None => cells.len(),
        };
        for col in 0..count {
            let cell = cells.get(col).unwrap_or(&blank);
            let look = match cursor {
                Some(at) if at == col => cell.look.reversed(bg),
                _ => cell.look,
            };
            let len = cell.ch.len_utf8();
            text.push(cell.ch);
            match (&mut runs.last_mut(), last) {
                (Some(run), Some(prev)) if prev == look => run.len += len,
                _ => runs.push(self.run(len, look)),
            }
            last = Some(look);
        }
        StyledText::new(text).with_runs(runs)
    }
}

/// How a cell is drawn: a theme style resolved over the base colours, in the
/// terms a text run takes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Look {
    fg: Hsla,
    bg: Option<Hsla>,
    bold: bool,
    italic: bool,
    underline: bool,
}

impl Look {
    fn plain(fg: Hsla) -> Self {
        Self { fg, bg: None, bold: false, italic: false, underline: false }
    }

    /// A theme style laid over this one: what it names wins, what it leaves
    /// unsaid stays.
    fn styled(self, s: ThemeStyle) -> Self {
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
    fn reversed(self, bg: Hsla) -> Self {
        self.reversed_over(Some(bg))
    }

    fn reversed_over(self, fallback: Option<Hsla>) -> Self {
        let bg = self.bg.or(fallback).unwrap_or(self.fg);
        Self { fg: bg, bg: Some(self.fg), ..self }
    }
}

/// One cell of a row: a character and how to draw it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cell {
    ch: char,
    look: Look,
}

/// A line as cells: tabs to the next stop, control characters as `^X`, the
/// C1 controls dropped — `tui::render::cells`, from column zero — with each
/// character's look carried onto every cell it became.
///
/// `looks` is one per char of `text`, or empty for a line with no
/// highlights; a short list means the rest is `base`.
fn expand(text: &str, looks: &[Look], base: Look, tab: usize) -> Vec<Cell> {
    let tab = tab.max(1);
    let mut out = Vec::with_capacity(text.len());
    for (i, ch) in text.chars().enumerate() {
        let look = looks.get(i).copied().unwrap_or(base);
        if ch == '\t' {
            let n = tab - (out.len() % tab);
            out.extend(std::iter::repeat_n(Cell { ch: ' ', look }, n));
        } else if let Some([a, b]) = glyph(ch) {
            out.push(Cell { ch: a, look });
            out.push(Cell { ch: b, look });
        } else if ch.is_control() {
        } else {
            out.push(Cell { ch, look });
        }
    }
    out
}

/// [`expand`] with no looks, for text that is all one colour.
fn cells(text: &str, base: Look, tab: usize) -> Vec<Cell> {
    expand(text, &[], base, tab)
}

/// The look of every char on one line, from the parse tree's spans over it.
///
/// `spans` are byte ranges in the whole buffer; `line_start` is the line's.
/// The first span to claim a byte keeps it, as in `tui::render::styled_line`.
fn looks(
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

fn keys(ev: &KeyDownEvent) -> Option<bi::key::Key> {
    crate::keys::translate(&ev.keystroke)
}

/// A theme colour as gpui's.
///
/// This function is the whole of what `gui/` knows about colour that `tui/`
/// does not: a terminal is handed the ANSI names and its own palette
/// answers, but a window has no palette to defer to, so the sixteen names
/// and the 256-colour cube get xterm's defaults.
fn color(c: ThemeColor) -> Hsla {
    let hex = match c {
        ThemeColor::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
        ThemeColor::Ansi(name) => ANSI[name as usize],
        ThemeColor::Indexed(n) => indexed(n),
    };
    rgb(hex).into()
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

/// A theme style's colours over a base, `reverse` applied.
fn colors(s: ThemeStyle, base_fg: Hsla, base_bg: Hsla) -> (Hsla, Hsla) {
    let fg = s.fg.map(color).unwrap_or(base_fg);
    let bg = s.bg.map(color).unwrap_or(base_bg);
    if s.reverse { (bg, fg) } else { (fg, bg) }
}

impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.installed != Some(self.editor.config_epoch()) {
            self.installed = Some(self.editor.config_epoch());
            self.input.set_keys(self.editor.config().keys.clone());
        }

        // The grid: the font's advance is the cell, so the window's pixels
        // become columns and rows exactly as a terminal's do.
        let font_id = window.text_system().resolve_font(&font(self.family));
        let cell_w = window.text_system().em_advance(font_id, px(FONT_SIZE)).unwrap_or(px(8.0));
        let cell_h = px(LINE_HEIGHT);
        let viewport = window.viewport_size();
        let cols = ((viewport.width / cell_w).floor() as u16).max(1);
        let rows = ((viewport.height / cell_h).floor() as u16).max(2);

        // Everything but the footer is the editor's; the one window gets it
        // all, less its own status row.
        let body = Rect::new(0, 0, cols, rows - 1);
        let panes = self.editor.layout(body, CHROME);
        for &(id, rect) in &panes {
            self.editor.size_window(
                id,
                rect.width as usize,
                rect.height.saturating_sub(1) as usize,
            );
        }
        let focus = self.editor.focus();
        let rect = panes.iter().find(|(id, _)| *id == focus).map(|(_, r)| *r).unwrap_or(body);

        // Wait for whatever will change on its own — the terminal's
        // `recv_timeout`, as a task that wakes this view.
        self.timer = self.editor.redraw_in().map(|wait| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
        });

        let ui = self.editor.theme().ui;
        let fg = ui.foreground.map(color).unwrap_or_else(|| rgb(DEFAULT_FG).into());
        let bg = ui.background.map(color).unwrap_or_else(|| rgb(DEFAULT_BG).into());
        let footer_cursor =
            matches!(self.editor.session.mode, Mode::Command(_) | Mode::Search { .. });

        let mut lines: Vec<AnyElement> = Vec::with_capacity(rows as usize);
        let mut status = String::new();
        let base = Look::plain(fg);
        if let Some(Pane::Text { text, buffer, syntax, options, .. }) = self.editor.pane(focus) {
            let theme = self.editor.theme();
            let tab = options.tab_width;
            let (scroll, left) = (text.scroll, text.left);
            let height = rect.height.saturating_sub(1) as usize;
            let cursor = text.selections.cursor();
            let (cursor_row, cursor_col) = (buffer.row_at(cursor), buffer.col_at(cursor));
            let last = (scroll + height).min(buffer.line_count());

            // One query for the whole visible range, then partition per line.
            // Bounded by pane height, never by file size.
            let rope = buffer.rope();
            let highlights = syntax.map(|syntax| {
                let from = rope.line_to_byte(scroll.min(rope.len_lines()));
                let to = rope.line_to_byte(last.min(rope.len_lines()));
                (syntax, syntax.highlights(rope, from..to))
            });

            for row in scroll..last {
                let raw = rope.line(row).to_string();
                let raw = raw.trim_end_matches(['\n', '\r']);
                let line_start = rope.line_to_byte(row);
                let line_looks = match &highlights {
                    Some((syntax, spans)) => {
                        let line_end = line_start + raw.len();
                        let mine = spans.partition_point(|s| s.end_byte <= line_start);
                        let past = spans[mine..].partition_point(|s| s.start_byte < line_end);
                        looks(raw, line_start, &spans[mine..mine + past], syntax, theme, base)
                    }
                    None => Vec::new(),
                };
                let expanded = expand(raw, &line_looks, base, tab);
                let visible: Vec<Cell> =
                    expanded.into_iter().skip(left).take(cols as usize).collect();
                let at = (row == cursor_row && !footer_cursor)
                    .then(|| display_col(raw, cursor_col, tab).saturating_sub(left));
                lines.push(self.row(&visible, at, base, bg).into_any_element());
            }

            // The window's status row, the terminal's arrangement: where the
            // cursor is, then what it is in.
            let name = buffer
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "[No Name]".into());
            let modified = if buffer.is_modified() { " [+]" } else { "" };
            status = format!(" {}:{}  {name}{modified}", cursor_row + 1, cursor_col + 1);
        }
        let (status_fg, status_bg) = colors(ui.statusline, fg, bg);

        // The footer: the `:` line or the search, else the message with the
        // pending keys and the mode pushed right.
        let pending = self.input.pending_display();
        let footer: AnyElement = match &self.editor.session.mode {
            Mode::Command(line) => {
                let text = format!(":{line}");
                let at = 1 + display_col(&line.to_string(), line.cursor(), 8);
                self.row(&cells(&text, base, 8), Some(at), base, bg).into_any_element()
            }
            Mode::Search { query, forward } => {
                let text = format!("{}{query}", if *forward { '/' } else { '?' });
                let text = cells(&text, base, 8);
                let at = text.len();
                self.row(&text, Some(at), base, bg).into_any_element()
            }
            mode => {
                let mode_style = match mode {
                    Mode::Insert => ui.mode_insert,
                    Mode::Pick | Mode::Label => ui.mode_pick,
                    _ => ui.mode_normal,
                };
                let (mode_fg, mode_bg) = colors(mode_style, fg, bg);
                div()
                    .flex()
                    .justify_between()
                    .child(self.row(&cells(&self.editor.session.status, base, 8), None, base, bg))
                    .child(div().flex().child(format!("{pending} ")).child(
                        div().bg(mode_bg).text_color(mode_fg).child(format!(" {} ", mode.label())),
                    ))
                    .into_any_element()
            }
        };

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| this.key(ev, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .font_family(self.family)
            .text_size(px(FONT_SIZE))
            .line_height(cell_h)
            .whitespace_nowrap()
            .child(div().flex_1().flex().flex_col().children(lines))
            .child(div().h(cell_h).bg(status_bg).text_color(status_fg).child(status))
            .child(div().h(cell_h).child(footer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cube_and_the_ramp_are_xterms() {
        assert_eq!(indexed(16), 0x000000);
        assert_eq!(indexed(196), 0xff0000, "the cube's red corner");
        assert_eq!(indexed(231), 0xffffff);
        assert_eq!(indexed(232), 0x080808);
        assert_eq!(indexed(255), 0xeeeeee);
        assert_eq!(indexed(9), ANSI[9]);
    }

    fn text(cells: &[Cell]) -> String {
        cells.iter().map(|c| c.ch).collect()
    }

    #[test]
    fn cells_expand_tabs_and_name_controls() {
        let base = Look::plain(gpui::white());
        assert_eq!(text(&cells("a\tb", base, 4)), "a   b");
        assert_eq!(text(&cells("\x01", base, 4)), "^A");
        assert_eq!(text(&cells("x\u{85}y", base, 4)), "xy", "a C1 control has no glyph, no width");
    }

    /// A tab is one char with one look, and every cell it becomes keeps it.
    #[test]
    fn a_look_rides_onto_every_cell_its_char_became() {
        let base = Look::plain(gpui::white());
        let red = Look::plain(gpui::red());
        let cells = expand("\tx", &[red, base], base, 4);
        assert_eq!(cells.len(), 5);
        assert!(cells[..4].iter().all(|c| c.look == red));
        assert_eq!(cells[4].look, base);
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
}
