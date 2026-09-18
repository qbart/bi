//! The window: one pane, the focused buffer, two status rows, and keys.
//!
//! Does what `tui::render::render` does, in the same order — cell size, layout,
//! text, status rows — with gpui's `Render` in place of ratatui's `draw`. The
//! rows themselves are built in [`crate::cells`]; this file is what turns a
//! row of cells into text runs, and what knows the window. See
//! `docs/specs/gui.md`.

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, FocusHandle, FontStyle, FontWeight, Hsla, IntoElement, KeyDownEvent,
    Render, StyledText, Task, TextRun, UnderlineStyle, Window, div, font, prelude::*, px, rgb,
};

use bi::buffer::Cursor;
use bi::decoration::Layer;
use bi::editor::{Editor, Mode, Pane};
use bi::indent::display_col;
use bi::input::Input;
use bi::region::Shape;
use bi::theme::Style as ThemeStyle;
use bi::window::{Chrome, Rect};

use crate::cells::{Cell, Look, Row, cells, color, expand, inline_shift, looks};

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
        let Some(key) = crate::keys::translate(&ev.keystroke) else { return };
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
    /// the cell at column `cursor`, if any. Past the end of the text the
    /// cursor sits on a blank cell, which is where insert mode puts it.
    /// Adjacent cells that look the same become one run: a run per cell
    /// would shape every glyph on its own.
    fn row(&self, cells: &[Cell], cursor: Option<usize>, base: Look, bg: Hsla) -> StyledText {
        let mut text = String::new();
        let mut runs: Vec<TextRun> = Vec::new();
        let mut last: Option<Look> = None;
        let mut col = 0usize;
        let width: usize = cells.iter().map(|c| c.width).sum();
        let pad = cursor.map_or(0, |at| (at + 1).saturating_sub(width));
        let blank = Cell { ch: ' ', look: base, width: 1 };
        for cell in cells.iter().chain(std::iter::repeat_n(&blank, pad)) {
            let under =
                cursor.is_some_and(|at| cell.width > 0 && col <= at && at < col + cell.width);
            let look = if under { cell.look.reversed(bg) } else { cell.look };
            col += cell.width;
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
        let ed = &self.editor;
        if let Some(Pane::Text { text, buffer, syntax, options, .. }) = ed.pane(focus) {
            let theme = ed.theme();
            let tab = options.tab_width;
            let (scroll, left) = (text.scroll, text.left);
            let (width, height) = (rect.width as usize, rect.height.saturating_sub(1) as usize);
            let selections = &text.selections;
            let cursor = selections.cursor();
            let (cursor_row, cursor_col) = (buffer.row_at(cursor), buffer.col_at(cursor));
            let last = (scroll + height).min(buffer.line_count());

            // One query for the whole visible range, then partition per line.
            // Bounded by pane height, never by file size — and the same rule
            // for everything drawn that is not buffer text.
            let rope = buffer.rope();
            let highlights = syntax.map(|syntax| {
                let from = rope.line_to_byte(scroll.min(rope.len_lines()));
                let to = rope.line_to_byte(last.min(rope.len_lines()));
                (syntax, syntax.highlights(rope, from..to))
            });
            let decorations = ed.decorations(focus, scroll..last);
            // Columns to the left of the text: the sign column, then the widest
            // line number plus a space — fixed across modes so the file does
            // not slide sideways as the cursor moves, and reserved whether or
            // not anything is in it. Zen takes them back — see docs/specs/zen.md.
            let zen = ed.session.zen;
            let total = buffer.line_count();
            let sign_width = if zen { 0 } else { options.gutter };
            let numbers = if zen { 0 } else { options.number_width(total) };
            let gutter = sign_width + numbers;
            let text_width = width.saturating_sub(gutter);
            let signs: HashMap<usize, (char, ThemeStyle)> = ed
                .gutter_signs(focus, scroll..last)
                .into_iter()
                .map(|(row, ch, style)| (row, (ch, style)))
                .collect();
            // The shape only applies to the pane that pressed `v`/`V`/`Ctrl-V`,
            // and this is that pane: the one window is the focused one.
            let shape = ed.visual();

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
                let start = rope.line_to_char(row);
                let chars = raw.chars().count();
                let mut r =
                    Row { cells: expand(raw, &line_looks, base, tab), row, raw, start, tab };

                // Under the selection: a guide or a swatch has to let a
                // selected line still look selected.
                r.decorate(&decorations, Layer::Under, base);

                // Search matches, under the selection so a selected match
                // still reads as selected.
                if options.hlsearch
                    && let Some(search) = &ed.session.last_search
                {
                    for (from, to) in
                        buffer.matches_in(start, start + chars, &search.pattern, search.whole_word)
                    {
                        let from = display_col(raw, from.saturating_sub(start), tab);
                        let to = display_col(raw, (to - start).min(chars), tab);
                        r.paint(from..to, ui.search);
                    }
                }

                // Selected columns on this row. Charwise includes the character
                // under the head; linewise covers the row whatever the columns
                // are, and is filled to the pane's edge below.
                let mut linewise = false;
                for selection in selections.all() {
                    if selection.is_collapsed() && shape.is_none() {
                        continue;
                    }
                    let (lo, hi) = selection.range();
                    let (first, end) =
                        (buffer.row_at(Cursor::at(lo)), buffer.row_at(Cursor::at(hi)));
                    if row < first || row > end {
                        continue;
                    }
                    let cols = match shape {
                        Some(Shape::Lines) => {
                            linewise = true;
                            0..display_col(raw, chars, tab).max(1)
                        }
                        Some(Shape::Block) => {
                            let (from, to) = ed.block_span_in(focus, row);
                            let (from, to) = (from - start, to - start);
                            display_col(raw, from, tab)
                                ..display_col(raw, to, tab).max(display_col(raw, from, tab) + 1)
                        }
                        _ => {
                            let from = lo.saturating_sub(start).min(chars);
                            let to = if row < end { chars } else { (hi - start + 1).min(chars) };
                            display_col(raw, from, tab)
                                ..display_col(raw, to, tab).max(display_col(raw, from, tab) + 1)
                        }
                    };
                    r.paint(cols, ui.selection);
                }

                // The cursor is drawn on the primary head below; the others
                // have to be painted or they are invisible.
                if selections.len() > 1 {
                    for (i, selection) in selections.all().iter().enumerate() {
                        if i == selections.primary_index() || buffer.row_at(selection.head) != row {
                            continue;
                        }
                        let col = display_col(raw, buffer.col_at(selection.head), tab);
                        r.paint(col..col + 1, ui.cursor_alt);
                    }
                }

                // Over everything: a letter you are about to press has to be
                // readable wherever it lands. Then the ones that make their
                // own cells, because every column above is a column of the
                // text as it stands.
                r.decorate(&decorations, Layer::Over, base);
                r.insert_inline(&decorations, base);

                // The horizontal scroll, after every pass that speaks in
                // absolute columns.
                r.scroll(left, base);

                // A linewise selection reaches the edge of the pane but stays
                // out of the gutter: the number column is not part of what
                // you selected.
                if linewise {
                    r.fill(ui.selection.bg, 0, text_width, base);
                }
                r.clip(text_width, base);

                // The gutter, in front of everything above — every pass so far
                // spoke in columns of the text area, and none of them has to
                // know how wide the numbers are. The sign column first, then
                // the number, or a blank where one is not due so the text
                // stays put.
                let mut head: Vec<Cell> = Vec::with_capacity(gutter);
                if sign_width > 0 {
                    match signs.get(&row) {
                        Some((sign, style)) => {
                            let text = format!("{sign}{}", " ".repeat(sign_width - 1));
                            head.extend(cells(&text, base.styled(*style), tab));
                        }
                        None => head.extend(cells(&" ".repeat(sign_width), base, tab)),
                    }
                }
                if numbers > 0 {
                    match options.number.label_for(row, cursor_row) {
                        Some(n) => {
                            let style =
                                if row == cursor_row { ui.gutter_current } else { ui.gutter };
                            let text = format!("{n:>width$} ", width = numbers - 1);
                            head.extend(cells(&text, base.styled(style), tab));
                        }
                        None => head.extend(cells(&" ".repeat(numbers), base, tab)),
                    }
                }
                head.append(&mut r.cells);
                r.cells = head;

                // The cursor line takes the whole row, numbers included, and
                // paints only what nothing else has claimed.
                if row == cursor_row {
                    r.fill(ui.cursorline.bg, 0, width, base);
                }

                let at = (row == cursor_row && !footer_cursor).then(|| {
                    let col = display_col(raw, cursor_col, tab);
                    gutter + (col + inline_shift(&decorations, row, col)).saturating_sub(left)
                });
                lines.push(self.row(&r.cells, at, base, bg).into_any_element());
            }

            // Past-the-end rows, so an empty buffer doesn't look like a hang.
            let filler = cells("~", base.styled(ui.filler), tab);
            while lines.len() < height {
                lines.push(self.row(&filler, None, base, bg).into_any_element());
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
                let at = text.iter().map(|c| c.width).sum();
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
