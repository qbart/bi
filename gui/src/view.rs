//! The window: every pane the layout hands out, a footer, and keys.
//!
//! Does what `tui::render::render` does, in the same order — cell size, layout,
//! one pane at a time with its status row, the footer — with gpui's `Render`
//! in place of ratatui's `draw`. The rows themselves are built in
//! [`crate::cells`]; this file is what turns a row of cells into text runs,
//! places panes in the window, and knows which pane kind draws how. See
//! `docs/specs/gui.md`.

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, FocusHandle, FontStyle, FontWeight, Hsla, IntoElement, KeyDownEvent,
    Pixels, Render, StyledText, Task, TextRun, UnderlineStyle, Window, div, font, prelude::*, px,
    rgb,
};

use bi::buffer::Cursor;
use bi::decoration::Layer;
use bi::editor::{Editor, Mode, Pane};
use bi::indent::display_col;
use bi::input::Input;
use bi::region::Shape;
use bi::results::Row as ResultRow;
use bi::selection::Selections;
use bi::theme::{Style as ThemeStyle, Ui};
use bi::tree::{ClipMode, Kind, Row as TreeRow};
use bi::window::{Chrome, ContentKind, Rect, WindowId};

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

/// The terminal's chrome, until this frontend has reasons of its own: one
/// column between side-by-side panes for the rule, none between stacked
/// ones, since each window's status row already separates those.
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

/// One pane's body, drawn: its rows as cells, and where the cursor is if this
/// is the pane that has it.
struct Body {
    rows: Vec<Vec<Cell>>,
    /// `(row, column)` within the body, for the focused pane only.
    cursor: Option<(usize, usize)>,
}

/// What every pane renderer is handed: the colours and the base look.
#[derive(Clone, Copy)]
struct Palette {
    ui: Ui,
    fg: Hsla,
    bg: Hsla,
    base: Look,
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

    /// A row as an element of exactly one line's height, so an empty row
    /// still takes its place.
    fn line(&self, cells: &[Cell], cursor: Option<usize>, p: Palette, h: Pixels) -> AnyElement {
        div().h(h).child(self.row(cells, cursor, p.base, p.bg)).into_any_element()
    }

    /// One text pane's body: `tui::render::render_window`, as cells.
    #[allow(clippy::too_many_arguments, reason = "a pane's geometry is this many facts")]
    fn text_body(
        &self,
        id: WindowId,
        text: &bi::window::Text,
        buffer: &bi::buffer::Buffer,
        syntax: Option<&bi::syntax::Syntax>,
        options: &bi::config::Options,
        width: usize,
        height: usize,
        focused: bool,
        p: Palette,
    ) -> Body {
        let ed = &self.editor;
        let (ui, base) = (p.ui, p.base);
        let theme = ed.theme();
        let tab = options.tab_width;
        let (scroll, left) = (text.scroll, text.left);
        let selections = &text.selections;
        let cursor = selections.cursor();
        let (cursor_row, cursor_col) = (buffer.row_at(cursor), buffer.col_at(cursor));
        let last = (scroll + height).min(buffer.line_count());
        // The cursor is drawn in the footer while the `:` or `/` line has it.
        let footer_cursor = matches!(ed.session.mode, Mode::Command(_) | Mode::Search { .. });

        // One query for the whole visible range, then partition per line.
        // Bounded by pane height, never by file size — and the same rule
        // for everything drawn that is not buffer text.
        let rope = buffer.rope();
        let highlights = syntax.map(|syntax| {
            let from = rope.line_to_byte(scroll.min(rope.len_lines()));
            let to = rope.line_to_byte(last.min(rope.len_lines()));
            (syntax, syntax.highlights(rope, from..to))
        });
        let decorations = ed.decorations(id, scroll..last);
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
            .gutter_signs(id, scroll..last)
            .into_iter()
            .map(|(row, ch, style)| (row, (ch, style)))
            .collect();
        // The shape only applies to the pane that pressed `v`/`V`/`Ctrl-V`;
        // in any other pane a collapsed cursor is a cursor, and a real range
        // is painted charwise, whatever mode the focused pane is in.
        let shape = if focused { ed.visual() } else { None };

        let mut rows = Vec::with_capacity(height);
        let mut at = None;
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
            let mut r = Row { cells: expand(raw, &line_looks, base, tab), row, raw, start, tab };

            // Under the selection: a guide or a swatch has to let a selected
            // line still look selected.
            r.decorate(&decorations, Layer::Under, base);

            // Search matches, under the selection so a selected match still
            // reads as selected.
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
                let (first, end) = (buffer.row_at(Cursor::at(lo)), buffer.row_at(Cursor::at(hi)));
                if row < first || row > end {
                    continue;
                }
                let cols = match shape {
                    Some(Shape::Lines) => {
                        linewise = true;
                        0..display_col(raw, chars, tab).max(1)
                    }
                    Some(Shape::Block) => {
                        let (from, to) = ed.block_span_in(id, row);
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

            // The cursor is drawn on the primary head; the others have to be
            // painted or they are invisible.
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
            // readable wherever it lands. Then the ones that make their own
            // cells, because every column above is a column of the text as
            // it stands.
            r.decorate(&decorations, Layer::Over, base);
            r.insert_inline(&decorations, base);

            // The horizontal scroll, after every pass that speaks in absolute
            // columns.
            r.scroll(left, base);

            // A linewise selection reaches the edge of the pane but stays out
            // of the gutter: the number column is not part of what you
            // selected.
            if linewise {
                r.fill(ui.selection.bg, 0, text_width, base);
            }
            r.clip(text_width, base);

            // The gutter, in front of everything above — every pass so far
            // spoke in columns of the text area, and none of them has to
            // know how wide the numbers are. The sign column first, then the
            // number, or a blank where one is not due so the text stays put.
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
                        let style = if row == cursor_row { ui.gutter_current } else { ui.gutter };
                        let text = format!("{n:>width$} ", width = numbers - 1);
                        head.extend(cells(&text, base.styled(style), tab));
                    }
                    None => head.extend(cells(&" ".repeat(numbers), base, tab)),
                }
            }
            head.append(&mut r.cells);
            r.cells = head;

            // The cursor line takes the whole row, numbers included, and
            // paints only what nothing else has claimed. Lit only in the
            // focused window, as vim's `'cursorline'` is.
            if row == cursor_row && focused {
                r.fill(ui.cursorline.bg, 0, width, base);
            }

            if row == cursor_row && focused && !footer_cursor {
                let col = display_col(raw, cursor_col, tab);
                let col =
                    gutter + (col + inline_shift(&decorations, row, col)).saturating_sub(left);
                at = Some((rows.len(), col.min(width.saturating_sub(1))));
            }
            rows.push(r.cells);
        }

        // Past-the-end rows, so an empty buffer doesn't look like a hang.
        while rows.len() < height {
            rows.push(cells("~", base.styled(ui.filler), tab));
        }
        Body { rows, cursor: at }
    }

    /// One window's tree: `tui::render::render_tree`, as cells.
    ///
    /// The glyphs are chosen here rather than in the core, which hands over
    /// depth and kind and nothing that looks like anything. No gutter and no
    /// highlighting: a row is not a line, and a path has no syntax.
    fn tree_body(
        &self,
        tree: &bi::tree::Tree,
        width: usize,
        height: usize,
        focused: bool,
        p: Palette,
    ) -> Body {
        let (ui, base) = (p.ui, p.base);
        let clipboard = &self.editor.session.clipboard;
        let all = tree.rows();
        let last = (tree.scroll() + height).min(all.len());
        let mut rows = Vec::with_capacity(height);
        let mut at = None;
        for (index, row) in all.iter().enumerate().take(last).skip(tree.scroll()) {
            // Per row, so a mixed clipboard shows which of its paths are
            // being copied and which are leaving.
            let mark = clipboard.mode_of(&row.path);
            let (indent, name) = tree_row_parts(row, mark);
            let style = match row.kind {
                Kind::Dir => ThemeStyle { bold: true, ..ui.tree_dir },
                Kind::Link => ui.tree_link,
                Kind::File => ThemeStyle::default(),
            };
            let mark_style = match mark {
                Some(ClipMode::Copy) => ThemeStyle { bold: true, ..ui.mark_copy },
                Some(ClipMode::Cut) => ThemeStyle { bold: true, ..ui.mark_cut },
                None => ThemeStyle::default(),
            };
            let mut line = cells(&indent, base.styled(mark_style), 8);
            line.extend(cells(&name, base.styled(style), 8));
            let mut r = Row { cells: line, row: index, raw: "", start: 0, tab: 8 };
            r.clip(width, base);
            if index == tree.selected() {
                // Shown in every tree pane, not only the focused one: unlike
                // a text cursor this is where the *next* Enter goes.
                let bg = if focused { ui.selection.bg } else { ui.cursorline.bg };
                r.fill(bg, 0, width, base);
                if focused {
                    let col = indent.chars().count().min(width.saturating_sub(1));
                    at = Some((rows.len(), col));
                }
            }
            rows.push(r.cells);
        }
        Body { rows, cursor: at }
    }

    /// One window's results: `tui::render::render_results`, as cells.
    fn results_body(
        &self,
        results: &bi::results::Results,
        width: usize,
        height: usize,
        focused: bool,
        p: Palette,
    ) -> Body {
        let (ui, base) = (p.ui, p.base);
        let tab = self.editor.session.options.tab_width;
        let all = results.rows();
        let last = (results.scroll() + height).min(all.len());
        let mut rows = Vec::with_capacity(height);
        let mut at = None;

        // Armed once, drawn per row: the pane previews every hit as it will
        // read, with the same matcher that found it.
        let armed = results
            .replace
            .as_ref()
            .and_then(|r| Some((r.with.clone(), bi::find_in_files::matcher(&results.query).ok()?)));

        for (index, row) in all.iter().enumerate().take(last).skip(results.scroll()) {
            let line: Vec<Cell> = match row {
                // A file leads its group, bold and in the tree's directory
                // colour: it is the same idea — a name with things under it.
                ResultRow::File { path, matches } => {
                    let mut line = cells(
                        &path.display().to_string(),
                        base.styled(ThemeStyle { bold: true, ..ui.tree_dir }),
                        tab,
                    );
                    line.extend(cells(&format!("  {matches}"), base.styled(ui.status_muted), tab));
                    line
                }
                ResultRow::Hit { index } => {
                    let m = &results.matches()[*index];
                    match &armed {
                        Some((with, matcher)) => {
                            // The line as it will read, the new text painted;
                            // an applied row keeps a ✓ as the record. A line
                            // the rewrite no longer matches is shown as it was.
                            let mut line =
                                cells(&format!("{:>6} ", m.line), base.styled(ui.gutter), tab);
                            match results.is_applied(*index) {
                                true => line.extend(cells("✓ ", base.styled(ui.mark_copy), tab)),
                                false => line.extend(cells("  ", base, tab)),
                            }
                            let prefix: usize = line.iter().map(|c| c.width).sum();
                            match bi::find_in_files::rewrite_line(
                                matcher,
                                &m.text,
                                with,
                                results.query.regex,
                            ) {
                                Some(rewrite) => {
                                    let text: Vec<char> = rewrite.text.chars().collect();
                                    let mut chars = vec![base; text.len()];
                                    for (from, to) in rewrite.spans {
                                        let (from, to) = (from.min(text.len()), to.min(text.len()));
                                        for look in &mut chars[from..to] {
                                            *look = look.styled(ui.search);
                                        }
                                    }
                                    let text: String = text.into_iter().collect();
                                    line.extend(expand_from(&text, &chars, base, tab, prefix));
                                }
                                None => line.extend(expand_from(&m.text, &[], base, tab, prefix)),
                            }
                            line
                        }
                        None => {
                            // The line number in the gutter's colour, so the
                            // eye can run down the numbers without the text
                            // getting in the way. The match itself is painted,
                            // which is what makes a row scannable when the
                            // line is long.
                            let mut line =
                                cells(&format!("{:>6}  ", m.line), base.styled(ui.gutter), tab);
                            let count = m.text.chars().count();
                            let (from, to) = (m.col.min(count), (m.col + m.len).min(count));
                            let mut chars = vec![base; count];
                            for look in &mut chars[from..to] {
                                *look = look.styled(ui.search);
                            }
                            line.extend(expand_from(&m.text, &chars, base, tab, 8));
                            line
                        }
                    }
                }
            };
            let mut r = Row { cells: line, row: index, raw: "", start: 0, tab };
            r.clip(width, base);
            if index == results.selected() {
                let bg = if focused { ui.selection.bg } else { ui.cursorline.bg };
                r.fill(bg, 0, width, base);
                if focused {
                    at = Some((rows.len(), 0));
                }
            }
            rows.push(r.cells);
        }
        Body { rows, cursor: at }
    }

    /// One window's status row: what it is showing, and where in it.
    ///
    /// The focused one leads with where the cursor is, because that is what
    /// you want from the window you are typing in; the others lead with what
    /// they are. The mode rides on the focused row, beside the position it
    /// applies to, and the git numstat sits to its left in each sign's colour.
    fn status_row(&self, id: WindowId, focused: bool, width: usize, p: Palette) -> Vec<Cell> {
        let ed = &self.editor;
        let ui = p.ui;
        let row = if focused { ui.statusline } else { ui.status_inactive };
        let (fg, bg) = colors(row, p.fg, p.bg);
        let look = Look { fg, bg: Some(bg), ..p.base };

        let (name, at) = match ed.pane(id) {
            None | Some(Pane::Tree { .. }) => return Vec::new(),
            Some(Pane::Results { results, .. }) => (
                results.title.clone(),
                format!("{} in {}", results.matches().len(), results.files()),
            ),
            Some(Pane::Image { img, .. }) => {
                let name = img
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| img.path.display().to_string());
                (name, format!("{}×{}", img.width, img.height))
            }
            Some(Pane::DapStack { stack, .. }) => {
                ("Stack".into(), format!("{} frames", stack.frames.len()))
            }
            Some(Pane::DapConsole { console, .. }) => {
                ("Console".into(), format!("{} lines", console.lines.len()))
            }
            Some(Pane::DapVariables { vars, .. }) => {
                ("Variables".into(), format!("{} rows", vars.visible().len()))
            }
            Some(Pane::DapWatches { list, .. }) => {
                ("Watches".into(), format!("{} watches", list.len()))
            }
            Some(Pane::Text { text, buffer, .. }) => {
                // The file name, not the path. Which `main.rs` it is belongs
                // to the picker; a pane thirty columns wide has no room to say
                // it twice.
                let name = buffer
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "[No Name]".into());
                let cursor = text.selections.cursor();
                // Storage badges only when there is something to say. A
                // non-UTF-8 encoding implies its BOM, so [bom] alone is the
                // UTF-8-with-BOM case. See docs/specs/encoding.md.
                let mut name = format!("{name}{}", if buffer.is_modified() { " [+]" } else { "" });
                let storage = buffer.storage();
                if storage.encoding_name() != "utf-8" {
                    name.push_str(&format!(" [{}]", storage.encoding_name()));
                } else if storage.bom {
                    name.push_str(" [bom]");
                }
                if storage.fileformat == bi::encoding::FileFormat::Dos {
                    name.push_str(" [crlf]");
                }
                (name, format!("{}:{}", buffer.row_at(cursor) + 1, buffer.col_at(cursor) + 1))
            }
        };
        let left = match focused {
            true => format!(" {at}  {name}"),
            false => format!(" {name}  {at}"),
        };
        let mut out = cells(&left, look, 8);

        // An image pane has no mode segment: modes do not exist there, and
        // the row should not claim otherwise.
        let image = matches!(ed.pane(id), Some(Pane::Image { .. }));
        let mut right: Vec<Cell> = Vec::new();
        if focused {
            if let Some(stats) = ed.git_stats(id).filter(|s| !s.is_clean()) {
                for (n, mark, style) in [
                    (stats.added, '+', ui.git_add),
                    (stats.changed, '~', ui.git_change),
                    (stats.removed, '-', ui.git_delete),
                ] {
                    if n > 0 {
                        let look = Look { fg: style.fg.map(color).unwrap_or(fg), ..look };
                        right.extend(cells(&format!("{mark}{n} "), look, 8));
                    }
                }
            }
            if !image {
                let mode_style = match ed.session.mode {
                    Mode::Insert => ui.mode_insert,
                    Mode::Pick | Mode::Label => ui.mode_pick,
                    _ => ui.mode_normal,
                };
                let (mfg, mbg) = colors(mode_style, p.fg, p.bg);
                let mode = Look { fg: mfg, bg: Some(mbg), ..p.base };
                right.extend(cells(&format!(" {} ", ed.session.mode.label()), mode, 8));
            }
        }
        let used: usize = out.iter().chain(&right).map(|c| c.width).sum();
        out.extend(std::iter::repeat_n(
            Cell { ch: ' ', look, width: 1 },
            width.saturating_sub(used),
        ));
        out.extend(right);
        let mut r = Row { cells: out, row: 0, raw: "", start: 0, tab: 8 };
        r.clip(width, look);
        r.cells
    }

    /// The footer: the `:` line or the search, else the message with the
    /// cursor count and the pending keys pushed right. The mode lives on the
    /// focused window's row; what is left here is the session's.
    fn footer(&self, width: usize, p: Palette) -> (Vec<Cell>, Option<usize>) {
        let ed = &self.editor;
        let (ui, base) = (p.ui, p.base);
        match &ed.session.mode {
            Mode::Command(line) => {
                let text = format!(":{line}");
                let at = 1 + display_col(&line.to_string(), line.cursor(), 8);
                (cells(&text, base, 8), Some(at))
            }
            Mode::Search { query, forward } => {
                let text = format!("{}{query}", if *forward { '/' } else { '?' });
                let text = cells(&text, base, 8);
                let at = text.iter().map(|c| c.width).sum();
                (text, Some(at))
            }
            _ => {
                let mut out = cells(" ", base, 8);
                out.extend(cells(&ed.session.status, base.styled(ui.status), 8));
                // Several cursors is a state you cannot otherwise tell from
                // the mode. Say how many.
                let cursors = ed.selections().map_or(0, Selections::len);
                let count =
                    if cursors > 1 { format!("{cursors} cursors  ") } else { String::new() };
                let keys = format!("{count}{}  ", self.input.pending_display());
                let keys = cells(&keys, base.styled(ui.status_muted), 8);
                let used: usize = out.iter().chain(&keys).map(|c| c.width).sum();
                let blank = Cell { ch: ' ', look: base, width: 1 };
                out.extend(std::iter::repeat_n(blank, width.saturating_sub(used)));
                out.extend(keys);
                (out, None)
            }
        }
    }
}

/// Everything to the left of a row's name: the mark column, the indent and
/// the open/closed marker. One mark column always, so marking something does
/// not shift the tree sideways under the cursor.
fn tree_row_parts(row: &TreeRow, mark: Option<ClipMode>) -> (String, String) {
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

/// [`expand`], with the tab stops counted from `col` rather than zero — for
/// text that continues a row something else began, so the stops land where
/// the buffer view would put them.
fn expand_from(text: &str, looks: &[Look], base: Look, tab: usize, col: usize) -> Vec<Cell> {
    let padded = format!("{}{text}", " ".repeat(col));
    let mut padded_looks = vec![base; col];
    padded_looks.extend_from_slice(looks);
    let mut out = expand(&padded, &padded_looks, base, tab);
    out.drain(..col);
    out
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

        // Everything but the footer is the editor's.
        let body = Rect::new(0, 0, cols, rows - 1);
        let panes = self.editor.layout(body, CHROME);
        let zen = self.editor.session.zen;
        // Every window is told its size before anything is drawn, so
        // scrolling has settled by the time the first pane is formatted. A
        // tree keeps its whole pane: it has no status row, because its first
        // row already names the root and a sidebar cannot spare a line to
        // say so twice. Zen gives the status row back to the text.
        for &(id, rect) in &panes {
            let chromeless = self.editor.content_kind_of(id) == Some(ContentKind::Tree) || zen;
            let height = if chromeless { rect.height } else { rect.height.saturating_sub(1) };
            self.editor.size_window(id, rect.width as usize, height as usize);
        }
        let focus = self.editor.focus();

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
        let p = Palette { ui, fg, bg, base: Look::plain(fg) };
        let at = |n: u16| n as f32;

        let mut elements: Vec<AnyElement> = Vec::with_capacity(panes.len() * 2);
        for &(id, rect) in &panes {
            let focused = id == focus;
            let chromeless = self.editor.content_kind_of(id) == Some(ContentKind::Tree) || zen;
            let (width, height) = (rect.width as usize, rect.height as usize);
            let body_height = if chromeless { height } else { height.saturating_sub(1) };

            let body = match self.editor.pane(id) {
                Some(Pane::Text { text, buffer, syntax, options, .. }) => self.text_body(
                    id,
                    text,
                    buffer,
                    syntax,
                    options,
                    width,
                    body_height,
                    focused,
                    p,
                ),
                Some(Pane::Tree { tree, .. }) => {
                    self.tree_body(tree, width, body_height, focused, p)
                }
                Some(Pane::Results { results, .. }) => {
                    self.results_body(results, width, body_height, focused, p)
                }
                // The image and debug panes are not drawn yet: their status
                // row says what they are, and their body says nothing rather
                // than something wrong. See the spec's *Not yet*.
                _ => Body { rows: Vec::new(), cursor: None },
            };

            let mut lines: Vec<AnyElement> = Vec::with_capacity(height);
            for (i, row) in body.rows.iter().enumerate() {
                let cursor = body.cursor.filter(|&(r, _)| r == i).map(|(_, c)| c);
                lines.push(self.line(row, cursor, p, cell_h));
            }
            if !chromeless {
                let status = self.status_row(id, focused, width, p);
                lines.push(self.line(&status, None, p, cell_h));
            }
            elements.push(
                div()
                    .absolute()
                    .left(cell_w * at(rect.x))
                    .top(cell_h * at(rect.y))
                    .w(cell_w * at(rect.width))
                    .h(cell_h * at(rect.height))
                    .flex()
                    .flex_col()
                    .children(lines)
                    .into_any_element(),
            );

            // The rule in the column the layout reserved to the left of this
            // pane.
            if rect.x > 0 {
                let bar = cells("│", p.base.styled(ui.rule), 8);
                let rows: Vec<AnyElement> =
                    (0..height).map(|_| self.line(&bar, None, p, cell_h)).collect();
                elements.push(
                    div()
                        .absolute()
                        .left(cell_w * at(rect.x - 1))
                        .top(cell_h * at(rect.y))
                        .w(cell_w)
                        .h(cell_h * at(rect.height))
                        .flex()
                        .flex_col()
                        .children(rows)
                        .into_any_element(),
                );
            }
        }

        let (footer, footer_cursor) = self.footer(cols as usize, p);

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
            .child(div().relative().flex_1().children(elements))
            .child(self.line(&footer, footer_cursor, p, cell_h))
    }
}
