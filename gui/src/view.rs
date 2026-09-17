//! The window: one pane, the focused buffer, two status rows, and keys.
//!
//! Does what `tui::render::render` does, in the same order — cell size, layout,
//! text, status rows — with gpui's `Render` in place of ratatui's `draw`. See
//! `docs/specs/gui.md`.

use gpui::{
    AnyElement, Context, FocusHandle, Hsla, IntoElement, KeyDownEvent, Render, StyledText, Task,
    TextRun, Window, div, font, prelude::*, px, rgb,
};

use bi::editor::{Editor, Mode, Pane};
use bi::indent::{char_width, display_col, glyph};
use bi::input::Input;
use bi::theme::{Color as ThemeColor, Style as ThemeStyle};
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

    fn run(&self, len: usize, color: Hsla, background: Option<Hsla>) -> TextRun {
        TextRun {
            len,
            font: font(self.family),
            color,
            background_color: background,
            underline: None,
            strikethrough: None,
        }
    }

    /// One row of cells, with the cursor drawn in reverse video over the cell
    /// at `cursor`, if any. Past the end of the text the cursor sits on a
    /// blank cell, which is where insert mode puts it.
    fn row(&self, text: &str, cursor: Option<usize>, fg: Hsla, bg: Hsla) -> StyledText {
        let Some(col) = cursor else {
            return StyledText::new(text.to_string()).with_runs(vec![self.run(
                text.len(),
                fg,
                None,
            )]);
        };
        let chars: Vec<char> = text.chars().collect();
        let before: String = chars.iter().take(col).collect();
        let cell = chars.get(col).copied().unwrap_or(' ').to_string();
        let after: String = chars.iter().skip(col + 1).collect();
        let runs = vec![
            self.run(before.len(), fg, None),
            self.run(cell.len(), bg, Some(fg)),
            self.run(after.len(), fg, None),
        ];
        StyledText::new(format!("{before}{cell}{after}")).with_runs(runs)
    }
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

/// A line as cells: tabs to the next stop, control characters as `^X`, the
/// C1 controls dropped. `tui::render::cells`, from column zero.
fn cells(text: &str, tab: usize) -> String {
    let tab = tab.max(1);
    let mut col = 0usize;
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\t' {
            let n = tab - (col % tab);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else if let Some([a, b]) = glyph(ch) {
            out.push(a);
            out.push(b);
            col += 2;
        } else if ch.is_control() {
        } else {
            out.push(ch);
            col += char_width(ch);
        }
    }
    out
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
        if let Some(Pane::Text { text, buffer, options, .. }) = self.editor.pane(focus) {
            let tab = options.tab_width;
            let (scroll, left) = (text.scroll, text.left);
            let height = rect.height.saturating_sub(1) as usize;
            let cursor = text.selections.cursor();
            let (cursor_row, cursor_col) = (buffer.row_at(cursor), buffer.col_at(cursor));
            let last = (scroll + height).min(buffer.line_count());

            for row in scroll..last {
                let raw = buffer.rope().line(row).to_string();
                let raw = raw.trim_end_matches(['\n', '\r']);
                let visible: String =
                    cells(raw, tab).chars().skip(left).take(cols as usize).collect();
                let at = (row == cursor_row && !footer_cursor)
                    .then(|| display_col(raw, cursor_col, tab).saturating_sub(left));
                lines.push(self.row(&visible, at, fg, bg).into_any_element());
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
                self.row(&cells(&text, 8), Some(at), fg, bg).into_any_element()
            }
            Mode::Search { query, forward } => {
                let text = format!("{}{query}", if *forward { '/' } else { '?' });
                let text = cells(&text, 8);
                let at = text.chars().count();
                self.row(&text, Some(at), fg, bg).into_any_element()
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
                    .child(cells(&self.editor.session.status, 8))
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

    #[test]
    fn cells_expand_tabs_and_name_controls() {
        assert_eq!(cells("a\tb", 4), "a   b");
        assert_eq!(cells("\x01", 4), "^A");
        assert_eq!(cells("x\u{85}y", 4), "xy", "a C1 control has no glyph and no width");
    }
}
