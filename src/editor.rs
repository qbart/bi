//! Editor state and the action dispatch table.
//!
//! [`Action`] is the seam. Today `input.rs` is the only thing that produces
//! actions and the keymap is hardcoded; when a config language shows up, it
//! produces actions too and nothing here changes.

use std::path::Path;

use anyhow::Result;

use crate::buffer::{Buffer, BufferId, Cursor};
use crate::history::Cursors;
use crate::motion::{Motion, Operator, Target, TextObject};
use crate::picker::{Item, Picker, PickerKind};
use crate::registers::{Entry, EntryKind, Registers, Sink};
use crate::selection::{Selection, Selections};
use crate::syntax::Syntax;
use crate::window::{Chrome, Layout, Rect, Window, WindowId};

/// Charwise, linewise or blockwise.
///
/// Blockwise is a rectangle rather than a range: the mode is the flag, the
/// primary selection's two corners are the rectangle, and the per-row spans
/// are derived on demand. See `docs/specs/blockwise.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualKind {
    Char,
    Line,
    Block,
}

/// What the gutter shows. `:set number`.
///
/// See `docs/specs/line-numbers.md`. The rules live here rather than in the
/// renderer because "what does row 12 show" is not a question about terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineNumbers {
    Off,
    Relative,
    /// Every `n`th line. `Every(1)` is plain numbering, and the default.
    Every(usize),
}

impl Default for LineNumbers {
    fn default() -> Self {
        Self::Every(1)
    }
}

impl LineNumbers {
    /// From the number the user typed. `None` for one that means nothing.
    pub fn from_setting(n: i64) -> Option<Self> {
        match n {
            0 => Some(Self::Off),
            -1 => Some(Self::Relative),
            n if n > 0 => Some(Self::Every(n as usize)),
            _ => None,
        }
    }

    /// The same number back, for `:set number` to report.
    pub fn setting(self) -> i64 {
        match self {
            Self::Off => 0,
            Self::Relative => -1,
            Self::Every(n) => n as i64,
        }
    }

    /// What to print beside `row`. `None` is a blank gutter cell.
    ///
    /// The cursor's own row always shows its absolute number: it is the one
    /// number a relative gutter cannot tell you, and the one `:{n}` needs.
    pub fn label_for(self, row: usize, cursor_row: usize) -> Option<usize> {
        if row == cursor_row {
            return (self != Self::Off).then_some(row + 1);
        }
        match self {
            Self::Off => None,
            Self::Relative => Some(row.abs_diff(cursor_row)),
            Self::Every(n) => ((row + 1) % n.max(1) == 0).then_some(row + 1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    /// Overwrites rather than inserts. `R`.
    Replace,
    /// Selections have room in them and motions move the head.
    Visual(VisualKind),
    /// The `:` line being typed, without the leading colon.
    Command(String),
    /// The `/` or `?` line being typed, without the leading key.
    Search {
        query: String,
        forward: bool,
    },
    /// The picker overlay is up. Its state lives in `Editor::picker` — a
    /// `Picker` is far too large to sit inside this enum.
    Pick,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Replace => "REPLACE",
            Mode::Visual(VisualKind::Char) => "VISUAL",
            Mode::Visual(VisualKind::Line) => "V-LINE",
            Mode::Visual(VisualKind::Block) => "V-BLOCK",
            Mode::Command(_) => "COMMAND",
            Mode::Search { .. } => "SEARCH",
            Mode::Pick => "PICK",
        }
    }

    /// Whether the cursor may rest one past the last char of a line.
    pub fn allows_eol(&self) -> bool {
        matches!(self, Mode::Insert | Mode::Replace)
    }

    pub fn visual(&self) -> Option<VisualKind> {
        match self {
            Mode::Visual(kind) => Some(*kind),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Move(Motion),
    /// An operator over the range a motion covers: `dw`, `c$`, `dd`.
    Operate {
        op: Operator,
        target: Target,
        /// Already folded — `2d3w` arrives here as 6.
        count: usize,
        sink: Sink,
    },
    /// `p` / `P`. Reads the front of the ring.
    Paste {
        before: bool,
        count: usize,
    },

    OpenPicker(PickerKind),
    PickChar(char),
    PickBackspace,
    PickNext,
    PickPrev,
    PickAccept,
    PickCancel,
    PickToggleShort,

    EnterInsert,
    EnterInsertAfter,
    EnterInsertLineStart,
    EnterInsertLineEnd,
    EnterNormal,
    /// `v` / `V`. The same key again leaves, as in vim.
    EnterVisual(VisualKind),
    /// `o` — swap which end of the selection the motions move.
    SwapEnds,
    /// `O` in blockwise — swap the columns and keep the rows.
    SwapCorners,
    /// `I` / `A` in blockwise — a cursor on every row of the block, then
    /// insert mode. Multi-cursor does the rest.
    BlockInsert {
        append: bool,
    },
    /// `r{char}` over a selection — every selected character, not just the one
    /// under the cursor.
    ReplaceSelection(char),
    /// An operator over whatever is selected. Visual mode's `d`, `c`, `y`.
    OperateSelection {
        op: Operator,
        sink: Sink,
    },
    /// `viw`, `vi(` — make the object the selection rather than operating on it.
    SelectObject {
        object: TextObject,
        around: bool,
    },
    /// `Ctrl-N` — a cursor at the next occurrence of the word under the cursor.
    AddCursorNextMatch,
    /// `Ctrl-Alt-Down` / `Ctrl-Alt-Up`.
    AddCursorLine {
        below: bool,
    },
    /// `Esc` in normal mode, when there is more than one cursor.
    CollapseCursors,
    /// `.` — repeat the last change. `count` replaces the original's.
    RepeatChange {
        count: Option<usize>,
    },

    /// `/` or `?`.
    ///
    /// Carries any pending operator, because entering the search line resets
    /// the keymap and `d/foo<CR>` would otherwise lose its `d`.
    EnterSearch {
        forward: bool,
        operator: Option<(Operator, Sink)>,
        count: usize,
    },
    SearchChar(char),
    SearchBackspace,
    SearchExecute,
    SearchCancel,
    /// `*` / `#` — search for the word under the cursor, whole-word.
    SearchWord {
        forward: bool,
    },

    /// `Ctrl-E` / `Ctrl-Y` — move the window, not the cursor.
    ScrollLine {
        down: bool,
    },
    /// `Ctrl-D` / `Ctrl-U` — move both, half a window at a time.
    ScrollHalfPage {
        down: bool,
    },
    /// `R`
    EnterReplace,
    /// A character typed in replace mode.
    ReplaceTyped(char),
    /// Backspace in replace mode: puts back what was overwritten.
    ReplaceBackspace,
    OpenLineBelow,
    OpenLineAbove,

    Undo,
    Redo,

    // These three fold their count in, like `Operate` and for the same reason:
    // the count is part of what the command means, not how many times to run
    // it. `3rx` replaces three characters once.
    /// `r{char}` — overwrite in place, without entering insert mode.
    ReplaceChar {
        ch: char,
        count: usize,
    },
    /// `~`
    ToggleCase {
        count: usize,
    },
    /// `J`
    JoinLines {
        count: usize,
    },

    InsertChar(char),
    InsertNewline,
    Backspace,

    EnterCommandMode,
    CommandChar(char),
    CommandBackspace,
    CommandExecute,
    CommandCancel,
}

impl Action {
    /// Whether a count means "do this N times" as opposed to being part of the
    /// action itself.
    fn repeatable(&self) -> bool {
        // A motion repeats, unless its count picks a destination instead.
        // `Operate` never repeats: it folded its counts in already.
        if let Action::Move(m) = self {
            return !m.is_absolute();
        }
        matches!(self, Action::InsertChar(_) | Action::Undo | Action::Redo)
    }
}

/// The last search, for `n`, `N` and the highlight pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub pattern: String,
    /// `*` and `#` only match whole words.
    pub whole_word: bool,
    /// The direction it was typed with, which is what `n` repeats.
    pub forward: bool,
}

/// How much text a visual operator covered, so `.` can repeat it from wherever
/// the cursor is now — the selection itself is gone by then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extent {
    Chars(usize),
    Lines(usize),
    /// A rectangle needs both dimensions, since neither implies the other.
    Block {
        rows: usize,
        cols: usize,
    },
}

/// The last change, in the form that replays it.
#[derive(Debug, Clone)]
struct Change {
    command: Command,
    /// Keystrokes of the insert or replace session it opened.
    ///
    /// `Action`s rather than a `String` because `Backspace` is part of what was
    /// typed, and it can cross the start of the insertion and eat text that was
    /// already there.
    typed: Vec<Action>,
    extent: Option<Extent>,
}

impl Action {
    /// Whether this action begins something `.` should repeat.
    ///
    /// Yank is deliberately absent: vim does not repeat it, because `.` after a
    /// yank almost always means "repeat the edit I made before the yank".
    fn starts_change(&self) -> bool {
        match self {
            Action::Operate { op, .. } | Action::OperateSelection { op, .. } => {
                *op != Operator::Yank
            }
            Action::Paste { .. }
            | Action::ReplaceChar { .. }
            | Action::ToggleCase { .. }
            | Action::JoinLines { .. }
            | Action::EnterInsert
            | Action::EnterInsertAfter
            | Action::EnterInsertLineStart
            | Action::EnterInsertLineEnd
            | Action::OpenLineBelow
            | Action::OpenLineAbove
            | Action::BlockInsert { .. }
            | Action::ReplaceSelection(_)
            | Action::EnterReplace => true,
            _ => false,
        }
    }

    /// Whether this action belongs to a search, and so leaves the status line
    /// in the search's hands.
    ///
    /// `n` and `N` are here because jumping through matches is still the
    /// search; `Esc` on the search line is not, because it abandons it.
    fn is_search(&self) -> bool {
        match self {
            Action::EnterSearch { .. }
            | Action::SearchChar(_)
            | Action::SearchBackspace
            | Action::SearchExecute
            | Action::SearchWord { .. } => true,
            Action::Move(Motion::Search { .. }) => true,
            Action::Operate { target: Target::Motion(Motion::Search { .. }), .. } => true,
            _ => false,
        }
    }

    /// Whether this action is a keystroke *within* a session rather than a
    /// command in its own right.
    fn is_session_key(&self) -> bool {
        matches!(
            self,
            Action::InsertChar(_)
                | Action::InsertNewline
                | Action::Backspace
                | Action::ReplaceTyped(_)
                | Action::ReplaceBackspace
        )
    }

    /// Whether the action leaves the buffer in a state `.` can replay from —
    /// i.e. whether it opens a session that has to be closed before the change
    /// is complete.
    fn opens_session(&self) -> bool {
        match self {
            Action::Operate { op, .. } | Action::OperateSelection { op, .. } => {
                *op == Operator::Change
            }
            Action::EnterInsert
            | Action::EnterInsertAfter
            | Action::EnterInsertLineStart
            | Action::EnterInsertLineEnd
            | Action::OpenLineBelow
            | Action::OpenLineAbove
            | Action::BlockInsert { .. }
            | Action::EnterReplace => true,
            _ => false,
        }
    }
}

fn cmd_of(action: Action) -> Command {
    Command { count: 1, action }
}

/// The same command with its count replaced, for `{n}.`.
fn with_count(command: Command, count: usize) -> Command {
    let action = match command.action {
        Action::Operate { op, target, sink, .. } => Action::Operate { op, target, count, sink },
        Action::Paste { before, .. } => Action::Paste { before, count },
        Action::ReplaceChar { ch, .. } => Action::ReplaceChar { ch, count },
        Action::ToggleCase { .. } => Action::ToggleCase { count },
        Action::JoinLines { .. } => Action::JoinLines { count },
        other => return Command { count, action: other },
    };
    Command { count: 1, action }
}

/// Moves a selection by `shift` characters, saturating at zero.
fn shifted(selection: Selection, shift: isize) -> Selection {
    let move_one = |c: Cursor| Cursor::at(c.at.saturating_add_signed(shift));
    Selection { anchor: move_one(selection.anchor), head: move_one(selection.head) }
}

#[derive(Debug, Clone)]
pub struct Command {
    pub count: usize,
    pub action: Action,
}

/// Editor state that belongs to the session rather than to any one file or
/// any one view of it — what a single keyboard has, regardless of what it is
/// pointed at.
///
/// A struct rather than a spray of fields on [`Editor`] so that [`View`] can
/// borrow all of it at once, disjointly from the buffer and the window it is
/// also holding. See `docs/specs/windows.md`.
#[derive(Default)]
pub struct Session {
    /// Not per-buffer: yanking in one file and pasting in another is the
    /// point, so the ring outlives any single buffer.
    pub registers: Registers,
    pub picker: Option<Picker>,
    pub mode: Mode,
    /// The last `f`/`F`/`t`/`T`, for `;` and `,` to repeat. Here rather than in
    /// the keymap because it must outlive `Input::reset()`.
    last_find: Option<Motion>,
    /// `$` in blockwise visual — the right edge is each row's own line end.
    ///
    /// A flag rather than a column because no column can say it. Vim calls the
    /// same thing `curswant = MAXCOL`.
    block_to_eol: bool,
    /// What replace mode has overwritten, newest last, one entry per selection
    /// per keystroke. Backspace pops it to put the original characters back.
    replaced: Vec<Vec<Option<char>>>,
    /// Selections as they were when the open undo group started. Empty between
    /// groups; filled by the first command that opens one.
    undo_from: Cursors,
    /// The change `.` replays, and the one being recorded.
    last_change: Option<Change>,
    recording: Option<Change>,
    /// Set while `.` is replaying, so the replay does not record itself and
    /// compound its own count.
    replaying: bool,
    /// The last search, for `n`/`N` and the highlight pass. Beside
    /// `last_find`, and there for the same reason: it outlives `Input::reset`.
    pub last_search: Option<Search>,
    /// Whether every match is highlighted. Off unless `:hls` asks for it —
    /// vim does not light the buffer up on a plain `/`, and the status line's
    /// `[3/17]` says how many there are without painting them.
    pub highlight_search: bool,
    /// What the gutter shows. `:set number`.
    ///
    /// Session-wide, though vim scopes `'number'` per window. One pane
    /// numbered and its neighbour not is a real thing to want, but it is an
    /// options problem, and the options table is still waiting on the config
    /// language — so this is the first field to claim window scope once there
    /// is somewhere to configure it from.
    pub line_numbers: LineNumbers,
    /// Whether the status line belongs to the search.
    ///
    /// True while the search line is being typed and for as long as the keys
    /// that follow are still the search — `n`, `N`, another `/`. The footer
    /// shows the pattern and the count and nothing else while it holds, which
    /// is what vim's command line does.
    pub search_focus: bool,
    /// Match positions for the status line's count, and what they were built
    /// from: the buffer, the pattern, whether it was a whole-word search, and
    /// that buffer's edit count. Any of the four moving makes it stale — the
    /// buffer included, or a count computed in one file would be served to
    /// another whose edit counter happened to agree.
    match_cache: Option<(BufferId, String, bool, u64, Vec<usize>)>,
    /// An operator waiting for the search line to be finished.
    pending_search_op: Option<(Operator, Sink, usize)>,
    pub status: String,
    pub quit: bool,
}

/// An open buffer, and what belongs to it rather than to a window.
struct BufferEntry {
    id: BufferId,
    buffer: Buffer,
    /// The parse tree, when the file's extension has a grammar.
    ///
    /// Beside the buffer rather than on it, because `pending_edits` has more
    /// than one consumer — tree-sitter now, LSP `didChange` later — and
    /// whoever drains it destroys it for the others. Putting the tree on
    /// `Buffer` would move the drain inside the buffer and break that.
    syntax: Option<Syntax>,
}

/// The session: every open buffer, every window onto them, and the state that
/// belongs to neither.
pub struct Editor {
    buffers: Vec<BufferEntry>,
    windows: Vec<Window>,
    layout: Layout,
    focus: WindowId,
    pub session: Session,
}

/// One buffer, one window, and the session, borrowed together for the length
/// of a command.
///
/// This is where the editing commands live. The borrow split is paid once here
/// rather than at every call site, and *which* window a command runs in is a
/// parameter rather than an assumption — see `docs/specs/windows.md`.
pub struct View<'a> {
    /// Which buffer `buffer` is, for the caches and lists that key on it.
    pub id: BufferId,
    pub buffer: &'a mut Buffer,
    pub syntax: &'a mut Option<Syntax>,
    pub window: &'a mut Window,
    pub session: &'a mut Session,
}

/// The block's left and right columns, from the corners of the primary
/// selection. Inclusive of the right, as charwise visual is.
///
/// Free functions rather than methods because the renderer asks these of a
/// window it is not editing, and building a `View` to answer would mean
/// borrowing the session mutably to read two columns.
fn block_columns_of(buffer: &Buffer, selections: &Selections) -> (usize, usize) {
    let selection = selections.primary();
    let a = buffer.col_at(selection.anchor);
    let b = buffer.col_at(selection.head);
    (a.min(b), a.max(b))
}

/// One `(start, end)` char range per row the block covers, top to bottom.
///
/// Rows too short to reach the left edge come back empty and stay in the list:
/// a block is a rectangle even where the text is not, and dropping them would
/// lose the shape a yanked block has to keep.
fn spans_of_block(buffer: &Buffer, selections: &Selections, to_eol: bool) -> Vec<(usize, usize)> {
    let (lo, hi) = selections.primary().range();
    let (first, last) = (buffer.row_at(Cursor::at(lo)), buffer.row_at(Cursor::at(hi)));
    (first..=last).map(|row| span_of_block_at(buffer, selections, to_eol, row)).collect()
}

/// The block's span on one row. What the renderer asks, a row at a time —
/// building the whole list per visible row would put the block's height into
/// the cost of a frame, which is the one thing rendering here avoids.
fn span_of_block_at(
    buffer: &Buffer,
    selections: &Selections,
    to_eol: bool,
    row: usize,
) -> (usize, usize) {
    let (left, right) = block_columns_of(buffer, selections);
    let start = buffer.rope().line_to_char(row);
    let len = buffer.line_len(row);
    let from = left.min(len);
    let to = if to_eol { len } else { (right + 1).min(len) };
    (start + from, start + to.max(from))
}

/// Picks a grammar from the file's extension. An unknown one yields `None`,
/// which renders as plain text.
fn syntax_for(buffer: &Buffer) -> Option<Syntax> {
    let extension = buffer
        .path
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_string();
    Syntax::new(&extension, buffer.rope())
}

impl Editor {
    pub fn empty() -> Self {
        Self::with_buffer(Buffer::empty())
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::with_buffer(Buffer::open(path)?))
    }

    fn with_buffer(buffer: Buffer) -> Self {
        let (buffer_id, window_id) = (BufferId(0), WindowId(0));
        Self {
            buffers: vec![BufferEntry { id: buffer_id, syntax: syntax_for(&buffer), buffer }],
            windows: vec![Window::new(window_id, buffer_id)],
            layout: Layout::new(window_id),
            focus: window_id,
            session: Session::default(),
        }
    }

    // ---- what the focused view is -------------------------------------------

    pub fn focus(&self) -> WindowId {
        self.focus
    }

    pub fn window(&self) -> &Window {
        self.window_of(self.focus).expect("focus always names a live window")
    }

    pub fn window_mut(&mut self) -> &mut Window {
        let focus = self.focus;
        self.window_mut_of(focus).expect("focus always names a live window")
    }

    pub fn window_of(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn window_mut_of(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// The focused window's buffer. Always valid: the buffer list is never
    /// empty and every window names a buffer in it.
    pub fn buffer(&self) -> &Buffer {
        &self.entry(self.window().buffer).buffer
    }

    pub fn buffer_mut(&mut self) -> &mut Buffer {
        let id = self.window().buffer;
        &mut self.entry_mut(id).buffer
    }

    pub fn syntax(&self) -> Option<&Syntax> {
        self.entry(self.window().buffer).syntax.as_ref()
    }

    pub fn selections(&self) -> &Selections {
        &self.window().selections
    }

    /// First visible row of the focused window.
    pub fn scroll(&self) -> usize {
        self.window().scroll
    }

    fn entry(&self, id: BufferId) -> &BufferEntry {
        self.buffers.iter().find(|b| b.id == id).expect("a window's buffer is always in the list")
    }

    fn entry_mut(&mut self, id: BufferId) -> &mut BufferEntry {
        self.buffers
            .iter_mut()
            .find(|b| b.id == id)
            .expect("a window's buffer is always in the list")
    }

    /// Borrows a buffer, its parse tree, a window and the session at once.
    ///
    /// Three disjoint fields of `self`, which is what lets the editing commands
    /// hold all of them mutably — the thing an accessor per field cannot do.
    pub fn view(&mut self, id: WindowId) -> View<'_> {
        let window = self
            .windows
            .iter_mut()
            .find(|w| w.id == id)
            .expect("view of a window that is not open");
        let entry = self
            .buffers
            .iter_mut()
            .find(|b| b.id == window.buffer)
            .expect("a window's buffer is always in the list");
        View {
            id: entry.id,
            buffer: &mut entry.buffer,
            syntax: &mut entry.syntax,
            window,
            session: &mut self.session,
        }
    }

    pub fn focused(&mut self) -> View<'_> {
        self.view(self.focus)
    }

    // ---- what the frontend and embedders call -------------------------------

    /// Lays the window tree out in `area` and returns one rect per window, in
    /// draw order.
    ///
    /// The frontend passes the area it owns and the chrome it intends to draw;
    /// how much of a pane is text is then its own decision, which it reports
    /// back through [`Editor::size_window`]. Keeping that split is what stops a
    /// status row — a terminal convention — from being baked into geometry.
    pub fn layout(&self, area: Rect, chrome: &Chrome) -> Vec<(WindowId, Rect)> {
        self.layout.rects(area, chrome)
    }

    /// Tells a window how much room it actually got, and scrolls it to its
    /// cursor. Called once per window per frame.
    pub fn size_window(&mut self, id: WindowId, width: usize, height: usize) {
        if let Some(window) = self.window_mut_of(id) {
            window.width = width;
        }
        self.view(id).scroll_to_cursor(height);
    }

    pub fn apply(&mut self, cmd: Command) {
        self.focused().apply(cmd);
    }

    /// Drains each buffer's edit log into its parse tree. Called once per key,
    /// after the command has been applied and before the frame is drawn.
    pub fn sync_syntax(&mut self) {
        for entry in &mut self.buffers {
            let edits = std::mem::take(&mut entry.buffer.pending_edits);
            if edits.is_empty() {
                continue;
            }
            if let Some(syntax) = &mut entry.syntax {
                syntax.update(entry.buffer.rope(), &edits);
            }
        }
    }

    pub fn cursor(&self) -> Cursor {
        self.selections().cursor()
    }

    pub fn cursor_row(&self) -> usize {
        self.buffer().row_at(self.cursor())
    }

    pub fn cursor_col(&self) -> usize {
        self.buffer().col_at(self.cursor())
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.focused().set_cursor(cursor);
    }

    pub fn block_spans(&self) -> Vec<(usize, usize)> {
        spans_of_block(self.buffer(), self.selections(), self.session.block_to_eol)
    }

    pub fn block_span_at(&self, row: usize) -> (usize, usize) {
        span_of_block_at(self.buffer(), self.selections(), self.session.block_to_eol, row)
    }

    pub fn search_count(&mut self) -> Option<(usize, usize)> {
        self.focused().search_count()
    }

    pub fn scroll_to_cursor(&mut self, height: usize) {
        self.focused().scroll_to_cursor(height);
    }

    #[cfg(test)]
    fn run_ex(&mut self, line: &str) {
        self.focused().run_ex(line);
    }
}

impl View<'_> {
    /// Rebuilds the parse tree from scratch. The old one belongs to text that
    /// no longer exists, and a new path can change the language outright.
    fn reload_syntax(&mut self) {
        *self.syntax = syntax_for(self.buffer);
    }

    pub fn apply(&mut self, cmd: Command) {
        if let Action::RepeatChange { count } = cmd.action {
            self.session.search_focus = false;
            self.repeat_change(count);
            return;
        }
        if self.session.undo_from.is_empty() {
            self.session.undo_from = self.window.selections.as_pairs();
        }
        self.record(&cmd);
        let n = if cmd.action.repeatable() { cmd.count.max(1) } else { 1 };
        for _ in 0..n {
            self.apply_once(&cmd.action);
        }
        // Decided per command rather than inside the search actions, because
        // what ends it is *anything else* — one place to say so, and no way
        // for a new action to forget. Set after the pass: `/` runs its motion
        // through a nested `apply`, and the outer command is the one that says
        // whether the search still owns the line.
        self.session.search_focus = cmd.action.is_search();

        // One command is one undo step, so the group closes here rather than
        // inside the count loop — `5x` comes back in a single `u`. Insert mode
        // and replace are the exceptions: the group stays open until Esc, which makes
        // a typing run (and the `\n` that `o` inserted before it) undo together.
        // One command is one undo step even across N selections, so the group
        // closes after the whole pass rather than per selection — otherwise `u`
        // would walk back through a multi-cursor edit one cursor at a time.
        //
        // Insert and replace are the exceptions: both hold the group open until
        // Esc, so a typing run — or a whole `R` session — comes back in one `u`.
        if !matches!(self.session.mode, Mode::Insert | Mode::Replace) {
            let after = self.window.selections.as_pairs();
            self.buffer.commit_undo(std::mem::take(&mut self.session.undo_from), after);
        }
    }

    /// Notes what `.` would replay.
    ///
    /// A change that opens a session — insert or replace — is not finished
    /// until the mode comes back to normal, which is what makes `ciwfoo<Esc>`
    /// one repeatable unit rather than five commands.
    fn record(&mut self, cmd: &Command) {
        if self.session.replaying {
            return;
        }

        if cmd.action.starts_change() {
            let extent = match cmd.action {
                // The selection is gone by the time `.` runs, so remember how
                // much it covered.
                Action::OperateSelection { .. } => Some(self.selection_extent()),
                _ => None,
            };
            let change = Change { command: cmd.clone(), typed: Vec::new(), extent };
            if cmd.action.opens_session() {
                self.session.recording = Some(change);
            } else {
                self.session.last_change = Some(change);
                self.session.recording = None;
            }
            return;
        }

        if let Some(recording) = &mut self.session.recording {
            if cmd.action.is_session_key() {
                recording.typed.push(cmd.action.clone());
            } else if matches!(cmd.action, Action::EnterNormal) {
                // The session is over, so the change is complete.
                self.session.last_change = self.session.recording.take();
            }
        }
    }

    fn selection_extent(&self) -> Extent {
        let selection = self.window.selections.primary();
        let (lo, hi) = selection.range();
        match self.session.mode.visual() {
            Some(VisualKind::Line) => {
                let rows = self.buffer.row_at(Cursor::at(hi)) - self.buffer.row_at(Cursor::at(lo));
                Extent::Lines(rows + 1)
            }
            Some(VisualKind::Block) => {
                let rows = self.buffer.row_at(Cursor::at(hi)) - self.buffer.row_at(Cursor::at(lo));
                let (left, right) = self.block_columns();
                Extent::Block { rows: rows + 1, cols: right + 1 - left }
            }
            _ => Extent::Chars(hi - lo + 1),
        }
    }

    fn block_columns(&self) -> (usize, usize) {
        block_columns_of(self.buffer, &self.window.selections)
    }

    pub fn block_spans(&self) -> Vec<(usize, usize)> {
        spans_of_block(self.buffer, &self.window.selections, self.session.block_to_eol)
    }

    pub fn block_span_at(&self, row: usize) -> (usize, usize) {
        span_of_block_at(self.buffer, &self.window.selections, self.session.block_to_eol, row)
    }

    /// The char at `(row, col)`, clamped to the row.
    fn at_row_col(&self, row: usize, col: usize) -> Cursor {
        let row = row.min(self.buffer.line_count() - 1);
        let start = self.buffer.rope().line_to_char(row);
        Cursor::at(start + col.min(self.buffer.line_len(row).saturating_sub(1)))
    }

    /// What is selected, as one span per row and never crossing a terminator.
    ///
    /// `r` is the caller: it overwrites characters, and a newline is not one it
    /// may overwrite. Blockwise is the interesting case — and it is the only
    /// one that is a single selection, so the others fold over the whole set.
    fn selection_spans(&self) -> Vec<(usize, usize)> {
        if self.session.mode.visual() == Some(VisualKind::Block) {
            return self.block_spans();
        }
        self.window.selections.all().iter().flat_map(|selection| self.rows_of(*selection)).collect()
    }

    /// One span per row a selection touches, clipped to that row's content.
    fn rows_of(&self, selection: Selection) -> Vec<(usize, usize)> {
        let (lo, hi) = match self.session.mode.visual() {
            Some(VisualKind::Line) => {
                self.buffer.line_range(selection.range().0, selection.range().1, false)
            }
            _ => selection.inclusive_range(self.buffer.rope().len_chars()),
        };
        let (first, last) =
            (self.buffer.row_at(Cursor::at(lo)), self.buffer.row_at(Cursor::at(hi.max(lo))));

        (first..=last)
            .map(|row| {
                let start = self.buffer.rope().line_to_char(row);
                let end = start + self.buffer.line_len(row);
                (start.max(lo), end.min(hi))
            })
            .filter(|(start, end)| end > start)
            .collect()
    }

    /// Cuts or copies the rectangle.
    ///
    /// Not routed through `for_each_selection`, because the selections it would
    /// iterate do not exist yet — the block is derived, and this is the moment
    /// it becomes real.
    fn operate_block(&mut self, op: Operator, sink: Sink) {
        let spans = self.block_spans();
        if sink == Sink::Ring {
            let rows: Vec<String> =
                spans.iter().map(|&(start, end)| self.buffer.slice(start, end)).collect();
            // One entry, not one per row: what was taken is a rectangle, and
            // pasting it back has to know that.
            self.session
                .registers
                .push(Entry { text: rows.join("\n"), kind: EntryKind::Blockwise });
        }

        let top_left = spans.first().map(|&(start, _)| start).unwrap_or(0);
        if op != Operator::Yank {
            // Bottom to top: a cut shifts everything below it and nothing
            // above, so descending order keeps every span's position valid
            // without a correction pass.
            for &(start, end) in spans.iter().rev() {
                if end > start {
                    self.buffer.operate_range(Cursor::at(start), op, start, end, false);
                }
            }
        }

        if op == Operator::Change {
            // A cursor per row rather than vim's replicate-on-Esc: the cursors
            // are real here, so the text lands on every row as it is typed.
            //
            // Each left edge has moved by everything cut above it. A row too
            // short to reach the block gets no cursor, as `I` skips it.
            self.session.mode = Mode::Insert;
            let mut removed = 0;
            let cursors: Vec<Selection> = spans
                .iter()
                .filter_map(|&(start, end)| {
                    let at = start - removed;
                    removed += end - start;
                    (end > start).then(|| Selection::at(at))
                })
                .collect();
            self.window.selections.set(cursors);
        } else {
            self.session.mode = Mode::Normal;
            self.window.selections =
                Selections::single(self.buffer.clamped(Cursor::at(top_left), false));
        }
    }

    /// Where the block's cursors go for `I` and `A`.
    ///
    /// `I` skips a row that does not reach the left edge; `A` pads one out to
    /// the column so what is appended lines up. Vim pads on `Esc`, bee pads on
    /// entry — the same edit, visible while it is being typed into.
    fn block_insert_columns(&mut self, append: bool) -> Vec<Cursor> {
        let (lo, hi) = self.window.selections.primary().range();
        let (first, last) =
            (self.buffer.row_at(Cursor::at(lo)), self.buffer.row_at(Cursor::at(hi)));
        let (left, right) = self.block_columns();

        let mut cursors = Vec::new();
        for row in first..=last {
            let len = self.buffer.line_len(row);
            let start = self.buffer.rope().line_to_char(row);
            if append {
                let col = if self.session.block_to_eol { len } else { right + 1 };
                if col > len {
                    let at = Cursor::at(start + len);
                    self.buffer.insert_str(at, &" ".repeat(col - len));
                }
                cursors.push(Cursor::at(self.buffer.rope().line_to_char(row) + col));
            } else if left <= len {
                cursors.push(Cursor::at(start + left));
            }
        }
        cursors
    }

    /// Replays the last change. `count`, when the user typed one on the `.`
    /// itself, replaces the original — `3x` then `2.` deletes two.
    fn repeat_change(&mut self, count: Option<usize>) {
        let Some(mut change) = self.session.last_change.clone() else {
            self.session.status = "nothing to repeat".into();
            return;
        };
        if let Some(count) = count {
            change.command = with_count(change.command, count);
        }

        self.session.replaying = true;

        match change.extent {
            // A visual operator repeats over the same extent from here, since
            // there is no selection any more.
            Some(extent) => self.repeat_over(&change, extent),
            None => self.apply(change.command.clone()),
        }
        for action in &change.typed {
            self.apply(Command { count: 1, action: action.clone() });
        }
        if change.command.action.opens_session() {
            self.apply(cmd_of(Action::EnterNormal));
        }

        self.session.replaying = false;
        // The replay is now the last change, so a second `.` repeats the same
        // thing rather than nothing.
        self.session.last_change = Some(change);
    }

    /// Re-selects `extent` from the cursor and applies the operator to it.
    fn repeat_over(&mut self, change: &Change, extent: Extent) {
        let Action::OperateSelection { op, sink } = change.command.action else { return };
        let kind = match extent {
            Extent::Chars(_) => VisualKind::Char,
            Extent::Lines(_) => VisualKind::Line,
            Extent::Block { .. } => VisualKind::Block,
        };
        self.session.mode = Mode::Visual(kind);
        self.for_each_selection(|ed, sel| {
            let head = match extent {
                Extent::Chars(n) => Cursor::at(sel.head.at + n - 1),
                Extent::Lines(n) => {
                    let row = ed.buffer.row_at(sel.head) + n - 1;
                    ed.buffer.at_row(row.min(ed.buffer.line_count() - 1), false)
                }
                // The far corner, not a distance along the text: a block is
                // cut from the same rectangle wherever it is repeated.
                Extent::Block { rows, cols } => {
                    let row =
                        (ed.buffer.row_at(sel.head) + rows - 1).min(ed.buffer.line_count() - 1);
                    let col = ed.buffer.col_at(sel.head) + cols - 1;
                    let line_start = ed.buffer.rope().line_to_char(row);
                    Cursor::at(line_start + col.min(ed.buffer.line_len(row).saturating_sub(1)))
                }
            };
            Selection { anchor: sel.head, head }
        });
        self.apply(cmd_of(Action::OperateSelection { op, sink }));
    }

    /// Runs `f` for every selection, highest position first.
    ///
    /// Two things have to be true and only the first is free.
    ///
    /// Descending order keeps every selection's *edit position* valid: an edit
    /// at 5 shifts what is above it, but one at 40 leaves 5 alone. So each
    /// selection is still pointing at the right text when its turn comes.
    ///
    /// The positions `f` hands back are a separate problem. A selection dealt
    /// with early sits above the ones still to come, so every later edit shifts
    /// it — the head it reported is stale the moment a lower edit lands. After
    /// each call the buffer's length delta is therefore applied to everything
    /// already processed. Skipping this is the bug where the second cursor of a
    /// multi-cursor insert ends up one character short per preceding cursor.
    fn for_each_selection(&mut self, mut f: impl FnMut(&mut View, Selection) -> Selection) {
        let mut list: Vec<Selection> = self.window.selections.all().to_vec();
        let mut order: Vec<usize> = (0..list.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(list[i].range().0));

        let mut done: Vec<usize> = Vec::with_capacity(list.len());
        for &i in &order {
            let before = self.buffer.rope().len_chars();
            list[i] = f(self, list[i]);
            let after = self.buffer.rope().len_chars();

            if after != before {
                let shift = after as isize - before as isize;
                for &j in &done {
                    list[j] = shifted(list[j], shift);
                }
            }
            done.push(i);
        }

        self.window.selections.set(list);
    }

    /// The selections to record as a revision's `before` and `after`.
    ///
    /// `undo_from` is captured at the start of each command and cleared when a
    /// group closes, so a group that spans several commands — a typing run —
    /// still reports where it started.
    fn undo_bounds(&mut self) -> (Cursors, Cursors) {
        let after = self.window.selections.as_pairs();
        (std::mem::take(&mut self.session.undo_from), after)
    }

    /// The primary cursor — what a single-cursor operation acts on.
    pub fn cursor(&self) -> Cursor {
        self.window.selections.cursor()
    }

    pub fn cursor_row(&self) -> usize {
        self.buffer.row_at(self.cursor())
    }

    pub fn cursor_col(&self) -> usize {
        self.buffer.col_at(self.cursor())
    }

    /// Collapses to a single cursor at `cursor`.
    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.window.selections = Selections::single(cursor);
    }

    /// Substitutes `;` / `,` with the find they repeat, and remembers any find
    /// that goes past.
    ///
    /// The last find lives here rather than in the keymap because it has to
    /// survive `Input::reset()`, which runs after every resolved command.
    /// `None` means "there is nothing to repeat" — the caller drops the whole
    /// action. It cannot resolve to some harmless motion instead: every motion
    /// means something, and a linewise one would make a bare `d;` delete the
    /// line.
    fn resolve_find(&mut self, motion: Motion) -> Option<Motion> {
        match motion {
            Motion::FindChar { .. } => {
                self.session.last_find = Some(motion);
                Some(motion)
            }
            Motion::Search { reverse } => {
                let search = self.session.last_search.clone()?;
                let forward = search.forward != reverse;
                // The echo follows the direction being travelled, not the one
                // that was typed: `N` after `/foo` is a `?foo`.
                self.echo_search(forward);
                let at = self.window.selections.cursor().at;
                let found = self.buffer.search(at, &search.pattern, forward, search.whole_word)?;
                // Resolved to an absolute destination: the pattern is gone by
                // the time `Buffer` sees it, so hand over a line-free jump.
                Some(Motion::Found(found))
            }
            Motion::RepeatFind { reverse } => match self.session.last_find {
                Some(Motion::FindChar { ch, forward, till, .. }) => {
                    Some(Motion::FindChar { ch, forward: forward != reverse, till, repeat: true })
                }
                _ => None,
            },
            _ => Some(motion),
        }
    }

    fn resolve_find_target(&mut self, target: Target) -> Option<Target> {
        match target {
            Target::Motion(m) => self.resolve_find(m).map(Target::Motion),
            object => Some(object),
        }
    }

    fn apply_once(&mut self, action: &Action) {
        let eol = self.session.mode.allows_eol();

        match action {
            Action::Move(m) => {
                let Some(m) = self.resolve_find(*m) else { return };
                // `$` in a block is a ragged right edge rather than a column,
                // and any other motion gives the edge back to the head.
                if self.session.mode.visual() == Some(VisualKind::Block) {
                    self.session.block_to_eol = m == Motion::LineEnd;
                }
                let visual = self.session.mode.visual().is_some();
                self.for_each_selection(|ed, sel| {
                    let head = ed.buffer.moved(sel.head, m, eol);
                    // In visual mode only the head moves; the anchor is what
                    // makes it a range rather than a cursor.
                    if visual {
                        Selection { anchor: sel.anchor, head }
                    } else {
                        Selection::collapsed(head)
                    }
                });
            }
            Action::Operate { op, target, count, sink } => {
                let Some(target) = self.resolve_find_target(*target) else { return };
                let (op, count, sink) = (*op, *count, *sink);
                self.for_each_selection(|ed, sel| {
                    match ed.buffer.operate(sel.head, op, target, count) {
                        Some((entry, landed)) => {
                            if sink == Sink::Ring {
                                ed.session.registers.push(entry);
                            }
                            Selection::collapsed(landed)
                        }
                        None => sel,
                    }
                });
                if op == Operator::Change {
                    self.session.mode = Mode::Insert;
                }
            }
            Action::Paste { before, count } => {
                // Cloned because pasting borrows the buffer mutably while the
                // entry is still owned by the ring.
                let Some(entry) = self.session.registers.front().cloned() else {
                    self.session.status = "nothing to paste".into();
                    return;
                };
                let (before, count) = (*before, *count);
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.paste(sel.head, &entry, before, count))
                });
            }

            Action::OpenPicker(kind) => self.open_picker(*kind),
            Action::PickChar(c) => {
                if let Some(p) = &mut self.session.picker {
                    p.push_char(*c);
                }
            }
            Action::PickBackspace => {
                // Backspacing off the front cancels, as it does on a `:` line.
                let empty = self.session.picker.as_mut().is_some_and(|p| !p.backspace());
                if empty {
                    self.close_picker();
                }
            }
            Action::PickNext => {
                if let Some(p) = &mut self.session.picker {
                    p.next();
                }
            }
            Action::PickPrev => {
                if let Some(p) = &mut self.session.picker {
                    p.prev();
                }
            }
            Action::PickToggleShort => {
                if let Some(p) = &mut self.session.picker {
                    p.toggle_short();
                }
            }
            Action::PickCancel => self.close_picker(),
            Action::PickAccept => self.accept_pick(),

            Action::EnterInsert => self.session.mode = Mode::Insert,
            Action::EnterInsertAfter => {
                // `a` may step onto the position just past the last char, which
                // normal mode forbids — so switch modes first.
                self.session.mode = Mode::Insert;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.moved(sel.head, Motion::Right, true))
                });
            }
            Action::EnterInsertLineStart => {
                self.session.mode = Mode::Insert;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.moved(sel.head, Motion::LineStart, true))
                });
            }
            Action::EnterInsertLineEnd => {
                self.session.mode = Mode::Insert;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.moved(sel.head, Motion::LineEnd, true))
                });
            }
            Action::EnterNormal => {
                // Leaving insert steps the cursor one *left*, back onto the
                // last character typed — not merely clamping it onto a valid
                // column. Vim does this, and it is what makes `iAB<Esc>.`
                // insert at the right place. Leaving visual mode does not.
                let stepping_back = matches!(self.session.mode, Mode::Insert | Mode::Replace);
                self.session.mode = Mode::Normal;
                self.session.replaced.clear();
                self.window.selections.collapse_each();
                self.for_each_selection(|ed, sel| {
                    let head = if stepping_back && ed.buffer.col_at(sel.head) > 0 {
                        Cursor::at(sel.head.at - 1)
                    } else {
                        sel.head
                    };
                    Selection::collapsed(ed.buffer.clamped(head, false))
                });
            }
            Action::OpenLineBelow | Action::OpenLineAbove => {
                let below = matches!(action, Action::OpenLineBelow);
                self.session.mode = Mode::Insert;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.open_line(sel.head, below))
                });
            }

            Action::ReplaceChar { ch, count } => {
                let (ch, count) = (*ch, *count);
                let mut refused = false;
                self.for_each_selection(|ed, sel| {
                    match ed.buffer.replace_chars(sel.head, ch, count) {
                        Some(landed) => Selection::collapsed(landed),
                        None => {
                            refused = true;
                            sel
                        }
                    }
                });
                if refused {
                    self.session.status = "not enough characters on the line".into();
                }
            }
            Action::ToggleCase { count } => {
                let count = *count;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.toggle_case(sel.head, count))
                });
            }
            Action::JoinLines { count } => {
                let count = *count;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.join_lines(sel.head, count))
                });
            }

            // Undo and redo are whole-buffer operations, not per-selection: the
            // history restores the position it recorded.
            // Whole-buffer operations, not per-selection: history restores the
            // selection set it recorded.
            Action::Undo => {
                let (before, after) = self.undo_bounds();
                match self.buffer.undo(before, after) {
                    Some(pairs) => self.restore(pairs),
                    None => self.session.status = "already at oldest change".into(),
                }
            }
            Action::Redo => {
                let (before, after) = self.undo_bounds();
                match self.buffer.redo(before, after) {
                    Some(pairs) => self.restore(pairs),
                    None => self.session.status = "already at newest change".into(),
                }
            }

            Action::InsertChar(c) => {
                let c = *c;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.insert_char(sel.head, c))
                });
            }
            Action::InsertNewline => {
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.insert_char(sel.head, '\n'))
                });
            }
            Action::Backspace => {
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.backspace(sel.head))
                });
            }

            Action::EnterVisual(kind) => {
                // The same key again leaves, as in vim.
                self.session.mode = if self.session.mode == Mode::Visual(*kind) {
                    self.window.selections.collapse_each();
                    Mode::Normal
                } else {
                    Mode::Visual(*kind)
                };
                if *kind == VisualKind::Block {
                    // The rectangle is derived from one selection's corners,
                    // so a block is single-selection by construction.
                    self.window.selections.collapse_to_primary();
                    self.session.block_to_eol = false;
                }
            }
            Action::SwapEnds => {
                self.for_each_selection(|_, sel| sel.flipped());
            }
            Action::SwapCorners => {
                let selection = self.window.selections.primary();
                let (anchor_row, head_row) =
                    (self.buffer.row_at(selection.anchor), self.buffer.row_at(selection.head));
                let (anchor_col, head_col) =
                    (self.buffer.col_at(selection.anchor), self.buffer.col_at(selection.head));
                let anchor = self.at_row_col(anchor_row, head_col);
                let head = self.at_row_col(head_row, anchor_col);
                *self.window.selections.primary_mut() = Selection { anchor, head };
                // The head chose the right edge, and it no longer does.
                self.session.block_to_eol = false;
            }
            Action::BlockInsert { append } => {
                let cursors = self.block_insert_columns(*append);
                self.session.mode = Mode::Insert;
                if cursors.is_empty() {
                    // Every row was too short for `I`. Nothing to insert into.
                    self.session.mode = Mode::Normal;
                    self.session.status = "no line reaches the block".into();
                    return;
                }
                self.window.selections.set(cursors.into_iter().map(Selection::collapsed).collect());
            }
            Action::ReplaceSelection(ch) => {
                let ch = *ch;
                let spans = self.selection_spans();
                // Length-preserving, so the order does not matter and no shift
                // correction is needed — unlike every other edit here.
                for (start, end) in spans {
                    self.buffer.replace_chars(Cursor::at(start), ch, end - start);
                }
                // Every selection collapses onto its own start, which keeps a
                // multi-cursor visual `r` multi-cursor.
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.clamped(Cursor::at(sel.range().0), false))
                });
                self.session.mode = Mode::Normal;
            }
            Action::OperateSelection { op, sink }
                if self.session.mode.visual() == Some(VisualKind::Block) =>
            {
                self.operate_block(*op, *sink);
            }
            Action::OperateSelection { op, sink } => {
                let (op, sink) = (*op, *sink);
                let linewise = self.session.mode.visual() == Some(VisualKind::Line);
                self.for_each_selection(|ed, sel| {
                    let len = ed.buffer.rope().len_chars();
                    let (start, end) = if linewise {
                        // Change keeps the line for insert mode to sit on, the
                        // same rule `cc` follows.
                        let (lo, hi) = sel.range();
                        ed.buffer.line_range(lo, hi, op != Operator::Change)
                    } else {
                        // Charwise visual includes the character under the head.
                        sel.inclusive_range(len)
                    };
                    match ed.buffer.operate_range(sel.head, op, start, end, linewise) {
                        Some((entry, landed)) => {
                            if sink == Sink::Ring {
                                ed.session.registers.push(entry);
                            }
                            Selection::collapsed(landed)
                        }
                        None => Selection::collapsed(sel.head),
                    }
                });
                self.session.mode =
                    if op == Operator::Change { Mode::Insert } else { Mode::Normal };
            }

            Action::SelectObject { object, around } => {
                let (object, around) = (*object, *around);
                self.for_each_selection(|ed, sel| {
                    match ed.buffer.object_range(sel.head, object, around) {
                        // The head sits *on* the last character, not past it:
                        // charwise visual is inclusive, so an exclusive end
                        // would select one character too many.
                        Some((start, end)) => Selection {
                            anchor: Cursor::at(start),
                            head: Cursor::at(end.saturating_sub(1).max(start)),
                        },
                        None => sel,
                    }
                });
            }

            Action::AddCursorNextMatch => {
                let primary = self.window.selections.primary();
                // The selection itself when there is one, otherwise the word
                // under the cursor — so it works in both normal and visual.
                let (start, end) = if primary.is_collapsed() {
                    match self.buffer.word_at(primary.head) {
                        Some(range) => range,
                        None => {
                            self.session.status = "no word under the cursor".into();
                            return;
                        }
                    }
                } else {
                    primary.inclusive_range(self.buffer.rope().len_chars())
                };

                let needle = self.buffer.slice(start, end);
                let Some(found) = self.buffer.find_next(primary.head.at, &needle) else {
                    self.session.status = format!("no more matches for \"{needle}\"");
                    return;
                };
                if found == start {
                    self.session.status = "only one match".into();
                    return;
                }
                let width = needle.chars().count();
                // A selection with room in it is only meaningful in visual
                // mode. In normal mode the new cursor goes to the *start* of
                // the match: collapsing the range onto its head would leave it
                // on the last character, so typing would land inside the word
                // rather than in front of it.
                self.window.selections.push(match self.session.mode.visual() {
                    Some(_) => {
                        Selection { anchor: Cursor::at(found), head: Cursor::at(found + width - 1) }
                    }
                    None => Selection::at(found),
                });
            }
            Action::AddCursorLine { below } => {
                let primary = self.window.selections.primary();
                let row = self.buffer.row_at(primary.head);
                let target = if *below { row + 1 } else { row.wrapping_sub(1) };
                if *below && target >= self.buffer.line_count() || !*below && row == 0 {
                    self.session.status = "no line there".into();
                    return;
                }
                // Keeps the column, which is what makes a column of cursors.
                let col = self.buffer.col_at(primary.head);
                let line_start = self.buffer.rope().line_to_char(target);
                let col = col.min(self.buffer.line_len(target).saturating_sub(1));
                self.window.selections.push(Selection::at(line_start + col));
            }
            Action::CollapseCursors => self.window.selections.collapse_to_primary(),
            // Intercepted in `apply`, which has to run before the undo group
            // and the change recorder see it.
            Action::RepeatChange { .. } => {}

            Action::EnterReplace => {
                self.session.mode = Mode::Replace;
                self.session.replaced.clear();
            }
            Action::ReplaceTyped(c) => {
                let c = *c;
                let mut overwritten = Vec::new();
                self.for_each_selection(|ed, sel| {
                    let row = ed.buffer.row_at(sel.head);
                    let col = ed.buffer.col_at(sel.head);
                    // Past the end of the line it appends, rather than eating
                    // the newline — which is what vim does.
                    let at_eol = col >= ed.buffer.line_len(row);
                    overwritten.push(if at_eol {
                        None
                    } else {
                        Some(ed.buffer.rope().char(sel.head.at))
                    });
                    let landed = if at_eol {
                        ed.buffer.insert_char(sel.head, c)
                    } else {
                        ed.buffer
                            .replace_chars(sel.head, c, 1)
                            .map(|landed| Cursor::at(landed.at + 1))
                            .unwrap_or(sel.head)
                    };
                    Selection::collapsed(landed)
                });
                self.session.replaced.push(overwritten);
            }
            Action::ReplaceBackspace => {
                let Some(overwritten) = self.session.replaced.pop() else { return };
                let mut i = 0;
                self.for_each_selection(|ed, sel| {
                    let original = overwritten.get(i).copied().flatten();
                    i += 1;
                    if sel.head.at == 0 {
                        return sel;
                    }
                    let back = Cursor::at(sel.head.at - 1);
                    let landed = match original {
                        // Put back what was overwritten rather than deleting.
                        Some(ch) => ed.buffer.replace_chars(back, ch, 1).unwrap_or(back),
                        // Nothing was overwritten here — it was an append.
                        None => ed.buffer.backspace(sel.head),
                    };
                    Selection::collapsed(landed)
                });
            }

            Action::EnterSearch { forward, operator, count } => {
                self.session.status.clear();
                self.session.pending_search_op = operator.map(|(op, sink)| (op, sink, *count));
                self.session.mode = Mode::Search { query: String::new(), forward: *forward };
            }
            Action::SearchChar(c) => {
                if let Mode::Search { query, .. } = &mut self.session.mode {
                    query.push(*c);
                }
            }
            Action::SearchBackspace => {
                // Backspacing off the front cancels, as it does on a `:` line.
                if let Mode::Search { query, .. } = &mut self.session.mode
                    && query.pop().is_none()
                {
                    self.cancel_search();
                }
            }
            Action::SearchCancel => self.cancel_search(),
            Action::SearchExecute => {
                let Mode::Search { query, forward } = &self.session.mode else { return };
                let (query, forward) = (query.clone(), *forward);
                self.session.mode = Mode::Normal;
                if query.is_empty() {
                    // A bare `/` repeats the last pattern, as in vim.
                    if self.session.last_search.is_none() {
                        self.session.pending_search_op = None;
                        return;
                    }
                } else {
                    self.session.last_search =
                        Some(Search { pattern: query, whole_word: false, forward });
                }
                self.run_search();
            }
            Action::SearchWord { forward } => {
                let at = self.window.selections.cursor();
                let Some((start, end)) = self.buffer.word_at(at) else {
                    self.session.status = "no word under the cursor".into();
                    return;
                };
                self.session.last_search = Some(Search {
                    pattern: self.buffer.slice(start, end),
                    whole_word: true,
                    forward: *forward,
                });
                self.run_search();
            }

            Action::ScrollLine { down } => self.scroll_by(if *down { 1 } else { -1 }, false),
            Action::ScrollHalfPage { down } => {
                let half = (self.window.height / 2).max(1) as isize;
                self.scroll_by(if *down { half } else { -half }, true);
            }

            Action::EnterCommandMode => {
                self.session.status.clear();
                self.session.mode = Mode::Command(String::new());
            }
            Action::CommandChar(c) => {
                if let Mode::Command(line) = &mut self.session.mode {
                    line.push(*c);
                }
            }
            Action::CommandBackspace => {
                if let Mode::Command(line) = &mut self.session.mode {
                    if line.pop().is_none() {
                        self.session.mode = Mode::Normal;
                    }
                }
            }
            Action::CommandCancel => self.session.mode = Mode::Normal,
            Action::CommandExecute => {
                let line = match &self.session.mode {
                    Mode::Command(line) => line.clone(),
                    _ => return,
                };
                self.session.mode = Mode::Normal;
                self.run_ex(&line);
            }
        }
    }

    /// Puts back the selections a revision recorded.
    ///
    /// The recorded pair is whatever was live when the change started, which
    /// for a visual or blockwise operator is a selection with room in it. That
    /// room means nothing in normal mode — it is drawn as a selection the user
    /// cannot act on, and vim leaves none after an undo — so outside visual
    /// mode each one collapses onto the start of what came back. The *number*
    /// of selections survives, which is what makes undoing a multi-cursor edit
    /// give the cursors back.
    fn restore(&mut self, pairs: Cursors) {
        self.window.selections = Selections::from_pairs(pairs);
        if self.session.mode.visual().is_some() {
            return;
        }
        self.for_each_selection(|ed, sel| {
            Selection::collapsed(ed.buffer.clamped(Cursor::at(sel.range().0), false))
        });
    }

    /// Which match the cursor is on and how many there are, for the status
    /// line. `None` when nothing has been searched for.
    ///
    /// The index is the match containing the cursor, or — when it sits on none
    /// — how many are behind it, so `[0/17]` means "before the first" exactly
    /// as it does in vim.
    pub fn search_count(&mut self) -> Option<(usize, usize)> {
        let search = self.session.last_search.clone()?;
        let token = self.buffer.edits();
        let stale = match &self.session.match_cache {
            // The buffer is part of the key: a count computed in one file must
            // not be served to another whose edit counter happens to agree.
            Some((id, pattern, whole_word, at, _)) => {
                *id != self.id
                    || *pattern != search.pattern
                    || *whole_word != search.whole_word
                    || *at != token
            }
            None => true,
        };
        if stale {
            let starts = self.buffer.match_starts(&search.pattern, search.whole_word);
            self.session.match_cache =
                Some((self.id, search.pattern.clone(), search.whole_word, token, starts));
        }

        let (_, _, _, _, starts) = self.session.match_cache.as_ref()?;
        let at = self.window.selections.cursor().at;
        let width = search.pattern.chars().count();
        let index = match starts.iter().position(|&start| at >= start && at < start + width) {
            Some(i) => i + 1,
            None => starts.iter().take_while(|&&start| start < at).count(),
        };
        Some((index, starts.len()))
    }

    /// What the status line echoes after a search — the pattern with the
    /// prefix of the direction it is *currently* going, so `N` after `/foo`
    /// reads `?foo`. Vim shows the command it would take to repeat the move.
    fn echo_search(&mut self, forward: bool) {
        if let Some(search) = &self.session.last_search {
            let prefix = if forward { '/' } else { '?' };
            self.session.status = format!("{prefix}{}", search.pattern);
        }
    }

    fn cancel_search(&mut self) {
        self.session.mode = Mode::Normal;
        self.session.pending_search_op = None;
    }

    /// Applies the last search as a motion, or as the target of the operator
    /// that was waiting for the search line to finish.
    fn run_search(&mut self) {
        let action = match self.session.pending_search_op.take() {
            Some((op, sink, count)) => Action::Operate {
                op,
                target: Target::Motion(Motion::Search { reverse: false }),
                count,
                sink,
            },
            None => Action::Move(Motion::Search { reverse: false }),
        };
        let found = self.resolve_find(Motion::Search { reverse: false }).is_some();
        if !found {
            let pattern =
                self.session.last_search.as_ref().map(|s| s.pattern.clone()).unwrap_or_default();
            self.session.status = format!("pattern not found: {pattern}");
            return;
        }
        self.apply(cmd_of(action));
    }

    /// Lines of context kept above and below the cursor.
    const SCROLLOFF: usize = 3;

    fn margin(height: usize) -> usize {
        Self::SCROLLOFF.min(height.saturating_sub(1) / 2)
    }

    /// Moves the window by `lines`, and the cursor with it when `follow` — or
    /// when the window would otherwise leave the cursor behind.
    fn scroll_by(&mut self, lines: isize, follow: bool) {
        let height = self.window.height;
        if height == 0 {
            return;
        }
        let last = self.buffer.line_count().saturating_sub(1);
        let max_scroll = self.buffer.line_count().saturating_sub(height);
        self.window.scroll = self.window.scroll.saturating_add_signed(lines).min(max_scroll);

        let row = self.buffer.row_at(self.window.selections.cursor());
        // The cursor has to end up inside the window *including* the scrolloff
        // margin. Leave it in the margin and `scroll_to_cursor` — which runs
        // every frame — immediately drags the window back, undoing the scroll.
        let margin = Self::margin(height);
        let top = (self.window.scroll + margin).min(last);
        let bottom = (self.window.scroll + height).saturating_sub(margin + 1).min(last);

        let wanted = if follow {
            // `Ctrl-D`/`Ctrl-U` keep the cursor's place within the window.
            row.saturating_add_signed(lines).min(last).clamp(top.min(bottom), bottom)
        } else {
            // `Ctrl-E`/`Ctrl-Y` move the cursor only when the window would
            // otherwise leave it outside.
            row.clamp(top.min(bottom), bottom)
        };
        if wanted != row {
            let cursor = self.buffer.at_row(wanted, false);
            self.set_cursor(cursor);
        }
    }

    fn open_picker(&mut self, kind: PickerKind) {
        if self.session.registers.is_empty() {
            // An empty overlay is a worse answer than saying so.
            self.session.status = "nothing to paste".into();
            return;
        }
        let items = self
            .session
            .registers
            .iter()
            .map(|e| Item {
                text: e.text.clone(),
                badge: match e.kind {
                    EntryKind::Linewise => Some('¶'),
                    EntryKind::Blockwise => Some('▚'),
                    EntryKind::Charwise => None,
                },
            })
            .collect();
        self.session.picker = Some(Picker::new(kind, items));
        self.session.mode = Mode::Pick;
    }

    fn close_picker(&mut self) {
        self.session.picker = None;
        self.session.mode = Mode::Normal;
    }

    fn accept_pick(&mut self) {
        let picker = self.session.picker.take();
        self.session.mode = Mode::Normal;
        let Some(picker) = picker else { return };
        let Some(entry) = picker.selected().and_then(|i| self.session.registers.get(i)).cloned()
        else {
            return;
        };
        match picker.kind {
            PickerKind::Register { before } => {
                // Push before pasting: move-to-front makes this the ring's head,
                // so `.` and a later bare `p` repeat the entry you chose rather
                // than whatever happened to be most recent.
                self.session.registers.push(entry.clone());
                let landed = self.buffer.paste(self.window.selections.cursor(), &entry, before, 1);
                self.window.selections = Selections::single(landed);
            }
        }
    }

    /// The `:` commands. Deliberately tiny — this is not where the editor gets
    /// interesting, and a real command table wants the config layer first.
    fn run_ex(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }

        let (cmd, arg) = match line.split_once(char::is_whitespace) {
            Some((c, a)) => (c, a.trim()),
            None => (line, ""),
        };
        let force = cmd.ends_with('!');
        let name = cmd.trim_end_matches('!');

        match name {
            // The `a` forms mean "every buffer" in vim. There is one buffer
            // here, so they are aliases — kept because the fingers that type
            // `:wa` type it everywhere, and they will still be right when a
            // buffer list exists.
            "w" | "write" | "wa" | "wall" => {
                self.write(arg);
            }
            "q" | "quit" | "qa" | "qall" => self.quit(force),
            "e" | "edit" => self.edit(arg, force),
            "noh" | "nohl" | "nohlsearch" => self.session.highlight_search = false,
            // Off by default, because a plain `/` in vim does not light up the
            // buffer. The count in the status line is what a search owes you.
            "hls" | "hlsearch" => self.session.highlight_search = true,
            "set" => self.set_option(arg),
            "wq" | "x" => {
                if self.write(arg) {
                    self.quit(true);
                }
            }
            _ => {
                if let Ok(n) = name.parse::<usize>() {
                    let cursor = self.buffer.at_row(n.saturating_sub(1), false);
                    self.window.selections = Selections::single(cursor);
                } else {
                    self.session.status = format!("not a command: {name}");
                }
            }
        }
    }

    /// `:set <option> <value>`, or `:set <option>=<value>` — vim's spelling,
    /// which the fingers type without asking.
    ///
    /// One option so far. A real options table wants the config layer this
    /// file has been waiting for; until then a match arm and an honest error
    /// for everything else is the whole of it.
    fn set_option(&mut self, arg: &str) {
        let (name, value) = match arg.split_once(['=', ' ']) {
            Some((name, value)) => (name.trim(), value.trim()),
            None => (arg.trim(), ""),
        };

        match name {
            "number" => {
                if value.is_empty() {
                    self.session.status = format!("number={}", self.session.line_numbers.setting());
                    return;
                }
                match value.parse::<i64>().ok().and_then(LineNumbers::from_setting) {
                    Some(lines) => self.session.line_numbers = lines,
                    None => {
                        self.session.status =
                            format!("number takes 0 (off), -1 (relative) or a count: {value}");
                    }
                }
            }
            "" => self.session.status = "set what?".into(),
            _ => self.session.status = format!("unknown option: {name}"),
        }
    }

    /// Returns whether the write succeeded.
    fn write(&mut self, path: &str) -> bool {
        let (before, after) = self.undo_bounds();
        let result = if path.is_empty() {
            self.buffer.save(before, after)
        } else {
            self.buffer.save_as(before, after, path)
        };
        match result {
            Ok(()) => {
                // `:w other.rs` can change the language under us.
                if !path.is_empty() {
                    self.reload_syntax();
                }
                let name =
                    self.buffer.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.session.status = format!("\"{name}\" written");
                true
            }
            Err(e) => {
                self.session.status = format!("error: {e:#}");
                false
            }
        }
    }

    /// `:e` reloads, `:e!` reloads discarding changes, `:e <path>` edits
    /// another file.
    ///
    /// The parse tree has to be rebuilt rather than patched: it belongs to text
    /// that no longer exists, and `<path>` can change the language outright.
    fn edit(&mut self, path: &str, force: bool) {
        if self.buffer.is_modified() && !force {
            self.session.status = "unsaved changes (use `:e!` to discard)".into();
            return;
        }

        let at = self.window.selections.cursor();
        let result = if path.is_empty() {
            self.buffer.reload(at)
        } else {
            Buffer::open(path).map(|buf| {
                *self.buffer = buf;
                // A different file, so the old position means nothing.
                Cursor::default()
            })
        };

        match result {
            Ok(cursor) => {
                self.window.selections = Selections::single(cursor);
                self.reload_syntax();
                let name =
                    self.buffer.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.session.status = format!("\"{name}\" loaded");
            }
            Err(e) => self.session.status = format!("{e:#}"),
        }
    }

    fn quit(&mut self, force: bool) {
        if self.buffer.is_modified() && !force {
            self.session.status = "unsaved changes (use `:q!` to discard)".into();
        } else {
            self.session.quit = true;
        }
    }

    /// Keeps the cursor inside a `height`-row viewport, with a scrolloff margin.
    pub fn scroll_to_cursor(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        self.window.height = height;
        let row = self.buffer.row_at(self.window.selections.cursor());
        let margin = Self::margin(height);

        if row < self.window.scroll + margin {
            self.window.scroll = row.saturating_sub(margin);
        } else if row + margin >= self.window.scroll + height {
            self.window.scroll = row + margin + 1 - height;
        }

        let max_scroll = self.buffer.line_count().saturating_sub(height);
        self.window.scroll = self.window.scroll.min(max_scroll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Cursor;
    use crate::motion::TextObject;
    use crate::picker::PickerKind;

    /// Text arrives as one committed revision, so a single undo lands back on
    /// it rather than on an empty buffer.
    fn editor(text: &str) -> Editor {
        let mut ed = Editor::empty();
        if !text.is_empty() {
            let from = ed.cursor();
            let at = ed.buffer_mut().insert_str(from, text);
            ed.set_cursor(at);
            let pairs = ed.selections().as_pairs();
            ed.buffer_mut().commit_undo(pairs.clone(), pairs);
        }
        ed.set_cursor(Cursor::at(0));
        ed
    }

    fn cmd(action: Action) -> Command {
        Command { count: 1, action }
    }

    fn type_str(ed: &mut Editor, text: &str) {
        for c in text.chars() {
            ed.apply(cmd(Action::InsertChar(c)));
        }
    }

    #[test]
    fn a_counted_command_undoes_as_one_unit() {
        let mut ed = editor("abcdef");
        ed.apply(operate_n(Operator::Delete, Motion::Right, 5));
        assert_eq!(ed.buffer().rope().to_string(), "f");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "abcdef", "5x is one unit, not five");
    }

    #[test]
    fn a_whole_insert_session_undoes_as_one_unit() {
        let mut ed = editor("");
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "hello");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().rope().to_string(), "hello");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "", "all five chars, not one");
    }

    /// `o` edits *and* enters insert mode. The newline it inserts belongs to the
    /// same undo unit as everything typed after it.
    #[test]
    fn open_line_and_what_follows_it_undo_together() {
        let mut ed = editor("a");
        ed.apply(cmd(Action::OpenLineBelow));
        type_str(&mut ed, "bc");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().rope().to_string(), "a\nbc");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "a", "the newline went back too");
    }

    #[test]
    fn entering_and_leaving_insert_without_typing_is_not_an_undo_step() {
        let mut ed = editor("a");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(cmd(Action::EnterInsert));
        ed.apply(cmd(Action::EnterNormal));

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "a", "one undo reaches the delete");
    }

    #[test]
    fn undo_takes_a_count() {
        let mut ed = editor("abcdef");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        assert_eq!(ed.buffer().rope().to_string(), "def");

        ed.apply(Command { count: 3, action: Action::Undo });
        assert_eq!(ed.buffer().rope().to_string(), "abcdef");
    }

    fn operate(op: Operator, motion: Motion, count: usize) -> Command {
        cmd(Action::Operate { op, target: Target::Motion(motion), count, sink: Sink::Ring })
    }

    /// `5x` — one command whose count the operator folded in.
    fn operate_n(op: Operator, motion: Motion, count: usize) -> Command {
        operate(op, motion, count)
    }

    #[test]
    fn dw_deletes_a_word_and_undoes_in_one_step() {
        let mut ed = editor("foo bar baz");
        ed.apply(operate(Operator::Delete, Motion::WordForward, 2));
        assert_eq!(ed.buffer().rope().to_string(), "baz");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "foo bar baz", "both words, one undo");
    }

    #[test]
    fn c_enters_insert_mode_so_you_can_type_the_replacement() {
        let mut ed = editor("foo bar");
        ed.apply(operate(Operator::Change, Motion::WordForward, 1));
        assert_eq!(ed.session.mode, Mode::Insert);
        assert_eq!(ed.buffer().rope().to_string(), " bar");

        type_str(&mut ed, "xyz");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().rope().to_string(), "xyz bar");
    }

    /// The change and everything typed into it are one undo step, the same rule
    /// that makes `o` plus its text one step.
    #[test]
    fn a_change_and_its_typing_undo_together() {
        let mut ed = editor("foo bar");
        ed.apply(operate(Operator::Change, Motion::WordForward, 1));
        type_str(&mut ed, "xyz");
        ed.apply(cmd(Action::EnterNormal));

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "foo bar");
    }

    #[test]
    fn a_delete_that_matches_nothing_leaves_no_undo_step() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::WordBackward, 1));
        assert_eq!(ed.buffer().rope().to_string(), "bc", "b at char 0 did nothing");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "abc", "one undo still reaches the x");
    }

    fn paste(before: bool, count: usize) -> Command {
        cmd(Action::Paste { before, count })
    }

    #[test]
    fn yank_then_paste_round_trips() {
        let mut ed = editor("foo bar");
        ed.apply(operate(Operator::Yank, Motion::WordForward, 1));
        assert_eq!(ed.buffer().rope().to_string(), "foo bar", "yank changed nothing");

        ed.apply(paste(false, 1));
        assert_eq!(ed.buffer().rope().to_string(), "ffoo oo bar");
    }

    #[test]
    fn a_delete_fills_the_ring_so_p_puts_it_back() {
        let mut ed = editor("one\ntwo");
        ed.apply(operate(Operator::Delete, Motion::CurrentLine, 1));
        assert_eq!(ed.buffer().rope().to_string(), "two");

        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer().rope().to_string(), "one\ntwo", "linewise, so above");
    }

    /// The whole point of `"_`: the text goes, the ring is untouched.
    #[test]
    fn the_black_hole_captures_nothing() {
        let mut ed = editor("keep\njunk");
        ed.apply(operate(Operator::Yank, Motion::CurrentLine, 1));

        ed.set_cursor(Cursor::at(5));
        ed.apply(Command {
            count: 1,
            action: Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::CurrentLine),
                count: 1,
                sink: Sink::BlackHole,
            },
        });
        assert_eq!(ed.buffer().rope().to_string(), "keep", "the junk line is gone");

        ed.apply(paste(false, 1));
        assert_eq!(
            ed.buffer().rope().to_string(),
            "keep\nkeep",
            "the ring still holds the yank, not the junk"
        );
    }

    #[test]
    fn pasting_from_an_empty_ring_says_so() {
        let mut ed = editor("abc");
        ed.apply(paste(false, 1));
        assert_eq!(ed.buffer().rope().to_string(), "abc");
        assert_eq!(ed.session.status, "nothing to paste");
    }

    #[test]
    fn a_paste_is_one_undo_step_even_with_a_count() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Yank, Motion::Right, 1));
        ed.apply(paste(false, 3));
        assert_eq!(ed.buffer().rope().to_string(), "aaaabc");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "abc");
    }

    /// Undo puts the text back in the buffer and leaves the ring alone, as vim
    /// does — you can undo a delete and still paste what it took.
    #[test]
    fn undo_does_not_roll_back_the_ring() {
        let mut ed = editor("one\ntwo");
        ed.apply(operate(Operator::Delete, Motion::CurrentLine, 1));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "one\ntwo");

        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer().rope().to_string(), "one\none\ntwo");
    }

    // ---- picker ------------------------------------------------------------

    fn pick_keys(ed: &mut Editor, actions: &[Action]) {
        for a in actions {
            ed.apply(cmd(a.clone()));
        }
    }

    fn open_register_picker(before: bool) -> Command {
        cmd(Action::OpenPicker(PickerKind::Register { before }))
    }

    /// Fills the ring with three distinct yanks, oldest first.
    fn ed_with_ring() -> Editor {
        let mut ed = editor("alpha\nbeta\ngamma");
        for row in 0..3 {
            let c = ed.buffer().at_row(row, false);
            ed.set_cursor(c);
            ed.apply(operate(Operator::Yank, Motion::CurrentLine, 1));
        }
        ed
    }

    #[test]
    fn the_picker_opens_over_the_ring_most_recent_first() {
        let mut ed = ed_with_ring();
        ed.apply(open_register_picker(false));

        assert_eq!(ed.session.mode, Mode::Pick);
        let p = ed.session.picker.as_ref().unwrap();
        assert_eq!(p.items()[p.selected().unwrap()].text, "gamma\n");
    }

    #[test]
    fn an_empty_ring_reports_instead_of_opening_an_empty_overlay() {
        let mut ed = editor("abc");
        ed.apply(open_register_picker(false));

        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.session.picker.is_none());
        assert_eq!(ed.session.status, "nothing to paste");
    }

    #[test]
    fn accepting_pastes_the_chosen_entry_not_the_most_recent() {
        let mut ed = ed_with_ring();
        let c = ed.buffer().at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickNext, Action::PickAccept]);

        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.session.picker.is_none());
        assert_eq!(
            ed.buffer().rope().to_string(),
            "alpha\nalpha\nbeta\ngamma",
            "the third-newest entry, chosen by moving down twice"
        );
    }

    #[test]
    fn typing_in_the_picker_filters_what_accept_takes() {
        let mut ed = ed_with_ring();
        let c = ed.buffer().at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickChar('b'), Action::PickChar('e'), Action::PickAccept]);
        assert_eq!(ed.buffer().rope().to_string(), "beta\nalpha\nbeta\ngamma");
    }

    /// Choosing promotes the entry, so a plain `p` afterwards repeats it — this
    /// is what makes `.` work without re-opening the picker.
    #[test]
    fn accepting_moves_the_entry_to_the_front_of_the_ring() {
        let mut ed = ed_with_ring();
        let c = ed.buffer().at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickNext, Action::PickAccept]);

        assert_eq!(ed.session.registers.front().unwrap().text, "alpha\n");
        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer().rope().to_string(), "alpha\nalpha\nalpha\nbeta\ngamma");
    }

    #[test]
    fn cancelling_leaves_the_buffer_and_the_ring_alone() {
        let mut ed = ed_with_ring();
        let before = ed.buffer().rope().to_string();
        ed.apply(open_register_picker(false));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickCancel]);

        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.session.picker.is_none());
        assert_eq!(ed.buffer().rope().to_string(), before);
        assert_eq!(ed.session.registers.front().unwrap().text, "gamma\n");
    }

    #[test]
    fn backspacing_an_empty_query_closes_the_picker() {
        let mut ed = ed_with_ring();
        ed.apply(open_register_picker(false));
        ed.apply(cmd(Action::PickChar('a')));
        ed.apply(cmd(Action::PickBackspace));
        assert_eq!(ed.session.mode, Mode::Pick, "still open, one char removed");

        ed.apply(cmd(Action::PickBackspace));
        assert_eq!(ed.session.mode, Mode::Normal, "nothing left to delete");
    }

    #[test]
    fn a_picked_paste_is_one_undo_step() {
        let mut ed = ed_with_ring();
        let before = ed.buffer().rope().to_string();
        let c = ed.buffer().at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickAccept]);
        assert_ne!(ed.buffer().rope().to_string(), before);

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), before);
    }

    #[test]
    fn redo_walks_back_forward() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(cmd(Action::Undo));
        ed.apply(cmd(Action::Redo));
        assert_eq!(ed.buffer().rope().to_string(), "bc");
    }

    #[test]
    fn the_ends_of_the_history_report_themselves() {
        let mut ed = editor("a");
        ed.apply(cmd(Action::Undo));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.session.status, "already at oldest change");

        ed.session.status.clear();
        ed.apply(cmd(Action::Redo));
        ed.apply(cmd(Action::Redo));
        assert_eq!(ed.session.status, "already at newest change");
    }

    // ---- :e ----------------------------------------------------------------

    /// A scratch file that cleans itself up.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str, text: &str) -> Self {
            let path = std::env::temp_dir().join(format!("bee-test-{}-{name}", std::process::id()));
            std::fs::write(&path, text).unwrap();
            Self(path)
        }
        fn write(&self, text: &str) {
            std::fs::write(&self.0, text).unwrap();
        }
        fn read(&self) -> String {
            std::fs::read_to_string(&self.0).unwrap()
        }
        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn opened(f: &Scratch) -> Editor {
        Editor::open(f.path()).unwrap()
    }

    fn ex(ed: &mut Editor, line: &str) {
        ed.run_ex(line);
    }

    #[test]
    fn e_rereads_the_file_from_disk() {
        let f = Scratch::new("reload.txt", "before\n");
        let mut ed = opened(&f);
        assert_eq!(ed.buffer().rope().to_string(), "before\n");

        f.write("after\n");
        ex(&mut ed, "e");
        assert_eq!(ed.buffer().rope().to_string(), "after\n");
    }

    #[test]
    fn e_refuses_to_discard_unsaved_changes() {
        let f = Scratch::new("guard.txt", "on disk\n");
        let mut ed = opened(&f);
        type_str(&mut ed, "local edit");
        f.write("changed underneath\n");

        ex(&mut ed, "e");
        assert!(ed.session.status.contains("unsaved changes"), "got: {}", ed.session.status);
        assert!(
            ed.buffer().rope().to_string().contains("local edit"),
            "the buffer must be left alone when the reload is refused",
        );
    }

    #[test]
    fn e_bang_discards_them() {
        let f = Scratch::new("force.txt", "on disk\n");
        let mut ed = opened(&f);
        type_str(&mut ed, "local edit");

        ex(&mut ed, "e!");
        assert_eq!(ed.buffer().rope().to_string(), "on disk\n");
        assert!(!ed.buffer().is_modified(), "a fresh read is not a modified buffer");
    }

    #[test]
    fn a_reload_drops_undo_history_rather_than_replaying_gone_text() {
        let f = Scratch::new("history.txt", "one\n");
        let mut ed = opened(&f);
        type_str(&mut ed, "typed");
        let pairs = ed.selections().as_pairs();
        ed.buffer_mut().commit_undo(pairs.clone(), pairs);

        f.write("two\n");
        ex(&mut ed, "e!");
        assert_eq!(ed.buffer().rope().to_string(), "two\n");

        // Undoing here must not resurrect text from the previous file.
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "two\n");
    }

    #[test]
    fn e_with_a_path_edits_that_file_instead() {
        let a = Scratch::new("a.txt", "file a\n");
        let b = Scratch::new("b.txt", "file b\n");
        let mut ed = opened(&a);

        ex(&mut ed, &format!("e {}", b.path()));
        assert_eq!(ed.buffer().rope().to_string(), "file b\n");
        assert_eq!(ed.buffer().path.as_deref(), Some(std::path::Path::new(b.path())));
    }

    #[test]
    fn a_shorter_file_does_not_leave_the_cursor_past_the_end() {
        let f = Scratch::new("shrink.txt", "one\ntwo\nthree\nfour\n");
        let mut ed = opened(&f);
        let c = ed.buffer().at_row(3, false);
        ed.set_cursor(c);

        f.write("x\n");
        ex(&mut ed, "e!");
        assert!(
            ed.cursor().at <= ed.buffer().rope().len_chars(),
            "cursor {} is past the end of a {}-char buffer",
            ed.cursor().at,
            ed.buffer().rope().len_chars(),
        );
        assert_eq!(ed.cursor_row(), 0);
    }

    #[test]
    fn a_reload_rebuilds_the_parse_tree_rather_than_patching_it() {
        let f = Scratch::new("tree.rs", "fn a() {}\n");
        let mut ed = opened(&f);
        assert!(ed.syntax().is_some(), "a .rs file should have a grammar");

        f.write("struct B;\n");
        ex(&mut ed, "e!");

        // A tree left over from the old text would disagree with the rope.
        let rope = ed.buffer().rope();
        let spans = ed.syntax().as_ref().unwrap().highlights(rope, 0..rope.len_bytes());
        assert!(
            spans.iter().all(|s| s.end_byte <= rope.len_bytes()),
            "highlight spans point past the end of the reloaded text",
        );
    }

    #[test]
    fn e_on_a_buffer_with_no_file_name_reports_rather_than_panicking() {
        let mut ed = editor("scratch");
        let pairs = ed.selections().as_pairs();
        ed.buffer_mut().commit_undo(pairs.clone(), pairs);
        ex(&mut ed, "e");
        assert!(!ed.session.status.is_empty(), "should say something");
        assert_eq!(ed.buffer().rope().to_string(), "scratch");
    }

    #[test]
    fn set_number_takes_off_relative_and_a_count() {
        let mut ed = editor("one\ntwo\nthree");
        assert_eq!(ed.session.line_numbers, LineNumbers::Every(1), "every line, by default");

        ex(&mut ed, "set number 0");
        assert_eq!(ed.session.line_numbers, LineNumbers::Off);
        ex(&mut ed, "set number -1");
        assert_eq!(ed.session.line_numbers, LineNumbers::Relative);
        ex(&mut ed, "set number 5");
        assert_eq!(ed.session.line_numbers, LineNumbers::Every(5));
        // Vim's spelling, which the fingers type without asking.
        ex(&mut ed, "set number=10");
        assert_eq!(ed.session.line_numbers, LineNumbers::Every(10));
    }

    #[test]
    fn set_reports_and_refuses_rather_than_guessing() {
        let mut ed = editor("one");
        ex(&mut ed, "set number 5");

        ex(&mut ed, "set number");
        assert_eq!(ed.session.status, "number=5", "no value asks rather than sets");

        ex(&mut ed, "set number -3");
        assert_eq!(ed.session.line_numbers, LineNumbers::Every(5), "left alone");
        assert!(ed.session.status.contains("-3"));

        ex(&mut ed, "set wrap");
        assert_eq!(ed.session.status, "unknown option: wrap");
    }

    #[test]
    fn relative_numbers_count_away_from_the_cursor_in_both_directions() {
        let lines = LineNumbers::Relative;
        assert_eq!(lines.label_for(10, 10), Some(11), "the cursor row shows where it is");
        assert_eq!(lines.label_for(7, 10), Some(3));
        assert_eq!(lines.label_for(13, 10), Some(3));
    }

    #[test]
    fn a_count_numbers_every_nth_line_and_the_one_the_cursor_is_on() {
        let lines = LineNumbers::Every(5);
        assert_eq!(lines.label_for(4, 0), Some(5), "line 5 is a multiple");
        assert_eq!(lines.label_for(5, 0), None, "line 6 is not");
        assert_eq!(lines.label_for(5, 5), Some(6), "except when the cursor is on it");
        assert_eq!(LineNumbers::Every(1).label_for(5, 0), Some(6), "1 numbers everything");
    }

    #[test]
    fn off_labels_nothing_at_all() {
        assert_eq!(LineNumbers::Off.label_for(3, 3), None, "not even the cursor row");
        assert_eq!(LineNumbers::Off.label_for(3, 0), None);
    }

    #[test]
    fn qa_and_wa_are_the_all_buffers_forms_of_q_and_w() {
        let f = Scratch::new("all.txt", "text\n");
        let mut ed = opened(&f);
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));

        ex(&mut ed, "qa");
        assert!(!ed.session.quit, "unsaved changes refuse `:qa` as they refuse `:q`");

        ex(&mut ed, "wa");
        assert_eq!(f.read(), "Xtext\n");
        assert!(!ed.buffer().is_modified());

        ex(&mut ed, "qall");
        assert!(ed.session.quit);
    }

    #[test]
    fn qa_bang_discards_like_q_bang() {
        let f = Scratch::new("force.txt", "text\n");
        let mut ed = opened(&f);
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));

        ex(&mut ed, "qa!");
        assert!(ed.session.quit);
        assert_eq!(f.read(), "text\n", "and the file is untouched");
    }

    // ---- commands across several selections --------------------------------
    //
    // Nothing binds a key to "add a cursor" yet — that is step 3 — but the
    // machinery is here, and these are the cases it exists to get right.

    fn with_cursors(text: &str, positions: &[usize]) -> Editor {
        let mut ed = editor(text);
        ed.window_mut().selections.set(positions.iter().map(|&p| Selection::at(p)).collect());
        ed
    }

    fn heads(ed: &Editor) -> Vec<usize> {
        ed.selections().all().iter().map(|s| s.head.at).collect()
    }

    #[test]
    fn typing_inserts_at_every_cursor() {
        //                   0123456789
        let mut ed = with_cursors("aa bb cc", &[0, 3, 6]);
        ed.session.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        assert_eq!(ed.buffer().rope().to_string(), "Xaa Xbb Xcc");
    }

    /// The reason edits run highest-position-first: an insert at 0 shifts
    /// everything after it, so an ascending pass would put the later ones in
    /// the wrong place.
    #[test]
    fn later_cursors_are_not_shifted_by_earlier_edits() {
        let mut ed = with_cursors("....|....|", &[4, 9]);
        ed.session.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('#')));
        assert_eq!(ed.buffer().rope().to_string(), "....#|....#|");
        assert_eq!(heads(&ed), vec![5, 11], "each cursor sits after what it typed");
    }

    #[test]
    fn a_motion_moves_every_cursor() {
        let mut ed = with_cursors("abc\ndef\nghi", &[0, 4]);
        ed.apply(cmd(Action::Move(Motion::Right)));
        assert_eq!(heads(&ed), vec![1, 5]);
    }

    #[test]
    fn an_operator_runs_at_every_cursor() {
        let mut ed = with_cursors("foo bar baz", &[0, 4, 8]);
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Object { object: TextObject::Word { big: false }, around: false },
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.buffer().rope().to_string(), "  ");
    }

    #[test]
    fn a_multi_cursor_edit_is_one_undo_step() {
        let mut ed = with_cursors("aa bb cc", &[0, 3, 6]);
        ed.session.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        ed.session.mode = Mode::Normal;
        ed.apply(cmd(Action::EnterNormal));

        assert_eq!(ed.buffer().rope().to_string(), "Xaa Xbb Xcc");
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "aa bb cc", "one u, not one per cursor",);
    }

    #[test]
    fn cursors_that_collide_merge_rather_than_typing_twice() {
        // Two cursors one apart; deleting forward brings them together.
        let mut ed = with_cursors("ab", &[0, 1]);
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Right),
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.selections().len(), 1, "collided cursors are one cursor");
    }

    #[test]
    fn the_primary_cursor_is_what_the_viewport_and_status_line_follow() {
        let ed = with_cursors("one\ntwo\nthree", &[0, 8]);
        assert_eq!(ed.cursor().at, ed.selections().primary().head.at);
        assert!(ed.cursor_row() <= 2);
    }

    /// Undo restores the whole selection set, not just one cursor. Without
    /// this, undoing a multi-cursor edit strands you with a single cursor and
    /// redo is unusable.
    #[test]
    fn undo_restores_every_cursor() {
        let mut ed = with_cursors("aa bb", &[0, 3]);
        ed.session.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().rope().to_string(), "Xaa Xbb");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "aa bb");
        assert_eq!(ed.selections().len(), 2, "both cursors come back");
        assert_eq!(heads(&ed), vec![0, 3]);
    }

    // ---- visual mode -------------------------------------------------------

    fn visual(text: &str, at: usize, kind: VisualKind) -> Editor {
        let mut ed = editor(text);
        ed.set_cursor(Cursor::at(at));
        ed.apply(cmd(Action::EnterVisual(kind)));
        ed
    }

    #[test]
    fn v_starts_a_selection_and_motions_move_only_the_head() {
        let mut ed = visual("hello world", 0, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        let sel = ed.selections().primary();
        assert_eq!(sel.anchor.at, 0, "the anchor stays where the selection began");
        assert_eq!(sel.head.at, 2);
    }

    #[test]
    fn the_same_key_again_leaves_visual_mode() {
        let mut ed = visual("hello", 0, VisualKind::Char);
        assert_eq!(ed.session.mode, Mode::Visual(VisualKind::Char));
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.selections().primary().is_collapsed());
    }

    #[test]
    fn v_then_big_v_switches_kind_rather_than_leaving() {
        let mut ed = visual("hello", 0, VisualKind::Char);
        ed.apply(cmd(Action::EnterVisual(VisualKind::Line)));
        assert_eq!(ed.session.mode, Mode::Visual(VisualKind::Line));
    }

    #[test]
    fn o_swaps_the_ends_so_the_other_one_can_be_adjusted() {
        let mut ed = visual("hello world", 2, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        let before = ed.selections().primary();
        ed.apply(cmd(Action::SwapEnds));
        let after = ed.selections().primary();
        assert_eq!(after.anchor.at, before.head.at);
        assert_eq!(after.head.at, before.anchor.at);
        assert_eq!(after.range(), before.range(), "the range is unchanged");
    }

    /// Charwise visual includes the character under the head, as in vim.
    #[test]
    fn a_charwise_operator_takes_the_character_under_the_head() {
        let mut ed = visual("hello", 0, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "llo", "both h and e");
        assert_eq!(ed.session.mode, Mode::Normal, "and it drops back to normal");
    }

    #[test]
    fn a_linewise_operator_takes_whole_lines_whatever_the_columns() {
        let mut ed = visual("one\ntwo\nthree", 5, VisualKind::Line);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "one\nthree");
    }

    #[test]
    fn a_visual_change_leaves_you_in_insert_mode() {
        let mut ed = visual("hello", 0, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Change, sink: Sink::Ring }));
        assert_eq!(ed.session.mode, Mode::Insert);
    }

    #[test]
    fn a_visual_yank_captures_without_changing_the_text() {
        let mut ed = visual("hello", 0, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "hello");
        assert_eq!(ed.session.registers.front().unwrap().text, "he");
    }

    #[test]
    fn a_linewise_yank_is_a_linewise_entry() {
        let mut ed = visual("one\ntwo", 0, VisualKind::Line);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        let entry = ed.session.registers.front().unwrap();
        assert_eq!(entry.kind, EntryKind::Linewise);
        assert!(entry.text.ends_with('\n'), "or pasting it could not open a line");
    }

    #[test]
    fn viw_makes_the_object_the_selection() {
        let mut ed = visual("foo bar baz", 5, VisualKind::Char);
        ed.apply(cmd(Action::SelectObject {
            object: TextObject::Word { big: false },
            around: false,
        }));
        let sel = ed.selections().primary();
        assert_eq!((sel.anchor.at, sel.head.at), (4, 6), "the head sits on the last char");

        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "foo  baz");
    }

    // ---- blockwise visual --------------------------------------------------

    /// A block from `at`, extended `rows` down and `cols` right — the two
    /// motions a `Ctrl-V` selection is made of.
    fn block(text: &str, at: usize, rows: usize, cols: usize) -> Editor {
        let mut ed = visual(text, at, VisualKind::Block);
        for _ in 0..rows {
            ed.apply(cmd(Action::Move(Motion::Down)));
        }
        for _ in 0..cols {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }
        ed
    }

    const GRID: &str = "abcdef\nghijkl\nmnopqr";

    #[test]
    fn a_block_is_one_span_per_row_between_its_corners() {
        let ed = block(GRID, 1, 2, 2);
        let text: Vec<String> =
            ed.block_spans().iter().map(|&(s, e)| ed.buffer().slice(s, e)).collect();
        assert_eq!(text, vec!["bcd", "hij", "nop"], "columns 1..3 of every row");
    }

    #[test]
    fn a_block_drawn_upwards_and_leftwards_is_the_same_rectangle() {
        let mut ed = visual(GRID, 19, VisualKind::Block); // 'r', bottom right
        for _ in 0..2 {
            ed.apply(cmd(Action::Move(Motion::Up)));
            ed.apply(cmd(Action::Move(Motion::Left)));
        }
        let text: Vec<String> =
            ed.block_spans().iter().map(|&(s, e)| ed.buffer().slice(s, e)).collect();
        assert_eq!(text, vec!["def", "jkl", "pqr"]);
    }

    #[test]
    fn deleting_a_block_cuts_the_same_columns_from_every_row() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "adef\ngjkl\nmpqr");
        assert_eq!(ed.cursor().at, 1, "and lands on the top-left corner");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn a_yanked_block_is_a_blockwise_entry_of_its_rows() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        let entry = ed.session.registers.front().unwrap();
        assert_eq!(entry.kind, EntryKind::Blockwise);
        assert_eq!(entry.text, "bc\nhi\nno", "rows joined, no terminator");
        assert_eq!(ed.buffer().rope().to_string(), GRID);
    }

    #[test]
    fn a_row_too_short_for_the_block_keeps_its_place_but_loses_nothing() {
        let mut ed = block("abcdef\ngh\nmnopqr", 3, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        assert_eq!(
            ed.session.registers.front().unwrap().text,
            "de\n\npq",
            "the short row is empty, not missing — the rectangle keeps its shape"
        );

        // A yank leaves visual mode, so the cut needs its own block.
        let mut ed = block("abcdef\ngh\nmnopqr", 3, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::BlackHole }));
        assert_eq!(ed.buffer().rope().to_string(), "abcf\ngh\nmnor");
    }

    #[test]
    fn dollar_takes_every_row_to_its_own_end() {
        let mut ed = block("abcdef\ngh\nmnopqr", 1, 2, 0);
        ed.apply(cmd(Action::Move(Motion::LineEnd)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "a\ng\nm", "ragged, not a column");
    }

    #[test]
    fn a_motion_after_dollar_gives_the_edge_back_to_the_head() {
        let mut ed = block("abcdef\ngh\nmnopqr", 1, 2, 0);
        ed.apply(cmd(Action::Move(Motion::LineEnd)));
        ed.apply(cmd(Action::Move(Motion::LineStart)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "acdef\ng\nmopqr");
    }

    #[test]
    fn changing_a_block_puts_a_cursor_on_every_row() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Change, sink: Sink::Ring }));
        assert_eq!(ed.session.mode, Mode::Insert);
        assert_eq!(ed.selections().len(), 3);
        type_str(&mut ed, "X");
        assert_eq!(ed.buffer().rope().to_string(), "aXdef\ngXjkl\nmXpqr");
    }

    #[test]
    fn block_insert_puts_a_cursor_at_the_left_edge_of_every_row() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::BlockInsert { append: false }));
        assert_eq!(ed.session.mode, Mode::Insert);
        type_str(&mut ed, "-");
        assert_eq!(ed.buffer().rope().to_string(), "a-bcdef\ng-hijkl\nm-nopqr");
    }

    #[test]
    fn block_insert_skips_a_row_that_does_not_reach_the_block() {
        let mut ed = block("abcdef\ngh\nmnopqr", 3, 2, 1);
        ed.apply(cmd(Action::BlockInsert { append: false }));
        type_str(&mut ed, "-");
        assert_eq!(ed.buffer().rope().to_string(), "abc-def\ngh\nmno-pqr");
    }

    #[test]
    fn block_append_pads_a_short_row_so_the_text_lines_up() {
        let mut ed = block("abcdef\ngh\nmnopqr", 3, 2, 1);
        ed.apply(cmd(Action::BlockInsert { append: true }));
        type_str(&mut ed, "|");
        assert_eq!(ed.buffer().rope().to_string(), "abcde|f\ngh   |\nmnopq|r");
    }

    #[test]
    fn block_append_after_dollar_lands_at_each_line_end() {
        let mut ed = block("abcdef\ngh\nmnopqr", 1, 2, 0);
        ed.apply(cmd(Action::Move(Motion::LineEnd)));
        ed.apply(cmd(Action::BlockInsert { append: true }));
        type_str(&mut ed, ";");
        assert_eq!(ed.buffer().rope().to_string(), "abcdef;\ngh;\nmnopqr;");
    }

    #[test]
    fn swapping_corners_keeps_the_rows_and_swaps_the_columns() {
        let mut ed = block(GRID, 1, 2, 2);
        ed.apply(cmd(Action::SwapCorners));
        let sel = ed.selections().primary();
        assert_eq!((ed.buffer().row_at(sel.anchor), ed.buffer().col_at(sel.anchor)), (0, 3));
        assert_eq!((ed.buffer().row_at(sel.head), ed.buffer().col_at(sel.head)), (2, 1));
        let text: Vec<String> =
            ed.block_spans().iter().map(|&(s, e)| ed.buffer().slice(s, e)).collect();
        assert_eq!(text, vec!["bcd", "hij", "nop"], "the same rectangle either way round");
    }

    #[test]
    fn r_over_a_block_overwrites_every_character_in_it() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::ReplaceSelection('.')));
        assert_eq!(ed.buffer().rope().to_string(), "a..def\ng..jkl\nm..pqr");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn r_over_a_charwise_selection_spans_lines_without_eating_the_newline() {
        let mut ed = visual("abc\ndef", 1, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::ReplaceSelection('.')));
        assert_eq!(ed.buffer().rope().to_string(), "a..\n..f");
    }

    #[test]
    fn undoing_a_block_delete_leaves_a_cursor_rather_than_a_selection() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        ed.apply(cmd(Action::Undo));

        assert_eq!(ed.buffer().rope().to_string(), GRID);
        assert_eq!(ed.selections().len(), 1);
        assert!(
            ed.selections().primary().is_collapsed(),
            "vim leaves no selection behind an undo, and normal mode cannot act on one"
        );
        assert_eq!(ed.cursor().at, 1, "on the start of what came back");
    }

    #[test]
    fn undoing_a_multi_cursor_edit_still_gives_the_cursors_back() {
        let mut ed = with_cursors("one two three", &[0, 4, 8]);
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "X");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().rope().to_string(), "Xone Xtwo Xthree");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "one two three");
        assert_eq!(ed.selections().len(), 3, "the cursors are part of what undo restores");
        assert!(ed.selections().all().iter().all(|s| s.is_collapsed()));
    }

    #[test]
    fn r_reaches_every_selection_when_there_is_more_than_one() {
        let mut ed = visual("foo bar foo", 0, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(ed.selections().len(), 2);
        ed.apply(cmd(Action::ReplaceSelection('.')));
        assert_eq!(ed.buffer().rope().to_string(), "... bar ...");
        assert_eq!(ed.selections().len(), 2, "and the cursors survive it");
    }

    #[test]
    fn a_block_is_single_selection_so_entering_one_drops_the_extra_cursors() {
        let mut ed = editor(GRID);
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        assert_eq!(ed.selections().len(), 2);
        ed.apply(cmd(Action::EnterVisual(VisualKind::Block)));
        assert_eq!(ed.selections().len(), 1);
    }

    #[test]
    fn pasting_a_block_puts_a_rectangle_back() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        ed.set_cursor(Cursor::at(4)); // row 0, column 4
        ed.apply(cmd(Action::Paste { before: false, count: 1 }));
        assert_eq!(ed.buffer().rope().to_string(), "abcdebcf\nghijkhil\nmnopqnor");
    }

    #[test]
    fn pasting_a_block_pads_short_rows_and_grows_the_buffer() {
        let mut ed = editor("xy");
        ed.session.registers.push(Entry { text: "bc\nhi\nno".into(), kind: EntryKind::Blockwise });
        ed.set_cursor(Cursor::at(1));
        ed.apply(cmd(Action::Paste { before: true, count: 1 }));
        assert_eq!(ed.buffer().rope().to_string(), "xbcy\n hi\n no");
    }

    #[test]
    fn a_repeated_block_operator_cuts_the_same_rectangle_again() {
        let mut ed = block(GRID, 1, 1, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "adef\ngjkl\nmnopqr");

        ed.set_cursor(Cursor::at(11)); // row 2, column 1
        ed.apply(cmd(Action::RepeatChange { count: None }));
        assert_eq!(ed.buffer().rope().to_string(), "adef\ngjkl\nmpqr", "one row, two columns");
    }

    // ---- replace mode ------------------------------------------------------

    fn replaced(text: &str, keys: &str) -> Editor {
        let mut ed = editor(text);
        ed.apply(cmd(Action::EnterReplace));
        for c in keys.chars() {
            ed.apply(cmd(Action::ReplaceTyped(c)));
        }
        ed
    }

    #[test]
    fn r_overwrites_rather_than_inserting() {
        let ed = replaced("abcdef", "XY");
        assert_eq!(ed.buffer().rope().to_string(), "XYcdef");
    }

    #[test]
    fn replace_past_the_end_of_the_line_appends() {
        // Vim does not let it eat the newline.
        let ed = replaced("ab\nnext", "XYZ");
        assert_eq!(ed.buffer().rope().to_string(), "XYZ\nnext");
    }

    /// The one thing `R` has that overwriting alone does not: Backspace puts
    /// the original characters back. Not testable through the vim differential
    /// harness, which inserts the DEL byte literally.
    #[test]
    fn backspace_in_replace_mode_restores_what_was_overwritten() {
        let mut ed = replaced("abcdef", "XY");
        assert_eq!(ed.buffer().rope().to_string(), "XYcdef");

        ed.apply(cmd(Action::ReplaceBackspace));
        assert_eq!(ed.buffer().rope().to_string(), "Xbcdef", "the b came back");
        ed.apply(cmd(Action::ReplaceBackspace));
        assert_eq!(ed.buffer().rope().to_string(), "abcdef", "and the a");
    }

    #[test]
    fn backspacing_past_an_appended_char_removes_it_rather_than_restoring() {
        let mut ed = replaced("ab", "XYZ");
        assert_eq!(ed.buffer().rope().to_string(), "XYZ");
        // The Z was appended past the end, so there is nothing to put back.
        ed.apply(cmd(Action::ReplaceBackspace));
        assert_eq!(ed.buffer().rope().to_string(), "XY");
    }

    #[test]
    fn leaving_replace_mode_forgets_what_it_overwrote() {
        let mut ed = replaced("abcdef", "XY");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.session.mode, Mode::Normal);
        // Nothing to pop, so this must not put stale characters back.
        ed.apply(cmd(Action::ReplaceBackspace));
        assert_eq!(ed.buffer().rope().to_string(), "XYcdef");
    }

    #[test]
    fn a_replace_session_is_one_undo_step() {
        let mut ed = replaced("abcdef", "XY");
        ed.apply(cmd(Action::EnterNormal));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "abcdef");
    }

    // ---- multi-cursor ------------------------------------------------------

    #[test]
    fn ctrl_n_puts_a_cursor_on_the_next_occurrence() {
        let mut ed = editor("foo bar foo baz foo");
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(ed.selections().len(), 2);
        assert_eq!(heads(&ed), vec![0, 8], "at the start of the match, not its end");

        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(heads(&ed), vec![0, 8, 16]);
    }

    /// The cursor must land on the *first* character of the match: collapsing
    /// the matched range onto its head would leave it on the last one, and
    /// typing would go inside the word.
    #[test]
    fn typing_with_several_cursors_lands_in_front_of_each_match() {
        let mut ed = editor("foo\nfoo\nfoo");
        ed.apply(cmd(Action::AddCursorNextMatch));
        ed.apply(cmd(Action::AddCursorNextMatch));
        ed.session.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        assert_eq!(ed.buffer().rope().to_string(), "Xfoo\nXfoo\nXfoo");
    }

    #[test]
    fn the_search_wraps_so_a_late_cursor_still_finds_the_first_match() {
        let mut ed = editor("foo bar foo");
        ed.set_cursor(Cursor::at(8));
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(heads(&ed), vec![0, 8], "wrapped to the one at the start");
    }

    #[test]
    fn a_word_with_no_other_occurrence_says_so_rather_than_adding_a_cursor() {
        let mut ed = editor("unique word");
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(ed.selections().len(), 1);
        assert!(!ed.session.status.is_empty());
    }

    #[test]
    fn ctrl_alt_down_adds_a_cursor_below_keeping_the_column() {
        let mut ed = editor("hello\nworld\nthere");
        ed.set_cursor(Cursor::at(3));
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        assert_eq!(ed.selections().len(), 2);
        assert_eq!(heads(&ed), vec![3, 9], "same column, next row");
    }

    #[test]
    fn adding_a_cursor_past_the_last_line_reports_rather_than_wrapping() {
        let mut ed = editor("only one line");
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        assert_eq!(ed.selections().len(), 1);
        assert!(!ed.session.status.is_empty());
    }

    #[test]
    fn a_cursor_below_clamps_to_a_shorter_line() {
        let mut ed = editor("longer line\nab");
        ed.set_cursor(Cursor::at(9));
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        let row1 = ed.buffer().rope().line_to_char(1);
        assert_eq!(heads(&ed), vec![9, row1 + 1], "clamped onto the short line");
    }

    #[test]
    fn esc_collapses_to_the_primary_cursor() {
        let mut ed = editor("foo foo foo");
        ed.apply(cmd(Action::AddCursorNextMatch));
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(ed.selections().len(), 3);

        ed.apply(cmd(Action::CollapseCursors));
        assert_eq!(ed.selections().len(), 1);
    }

    #[test]
    fn operators_run_at_every_cursor() {
        let mut ed = editor("foo a\nfoo b\nfoo c");
        ed.apply(cmd(Action::AddCursorNextMatch));
        ed.apply(cmd(Action::AddCursorNextMatch));
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Object { object: TextObject::Word { big: false }, around: true },
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.buffer().rope().to_string(), "a\nb\nc");
    }

    #[test]
    fn ctrl_n_in_visual_mode_selects_the_next_occurrence_of_the_selection() {
        let mut ed = editor("abc abc");
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::AddCursorNextMatch));

        assert_eq!(ed.selections().len(), 2);
        assert!(
            ed.selections().all().iter().all(|s| !s.is_collapsed()),
            "both are ranges, because visual mode is about ranges",
        );
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), " ");
    }

    // ---- `.` ---------------------------------------------------------------

    fn dot(ed: &mut Editor) {
        ed.apply(cmd(Action::RepeatChange { count: None }));
    }

    #[test]
    fn dot_repeats_an_immediate_change() {
        let mut ed = editor("abcdef");
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Right),
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.buffer().rope().to_string(), "bcdef");
        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "cdef");
        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "def", "and again");
    }

    #[test]
    fn dot_replays_a_whole_insert_session_as_one_unit() {
        let mut ed = editor("hello");
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "AB");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().rope().to_string(), "ABhello");

        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "AABBhello");
    }

    /// The reason `typed` holds actions rather than a string: a backspace can
    /// cross the start of the insertion and eat text that was already there.
    #[test]
    fn dot_replays_backspaces_within_the_session() {
        let mut ed = editor("hello");
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "ab");
        ed.apply(cmd(Action::Backspace));
        type_str(&mut ed, "c");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().rope().to_string(), "achello");

        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "aacchello");
    }

    #[test]
    fn a_motion_or_a_yank_does_not_become_the_thing_dot_repeats() {
        let mut ed = editor("one two three");
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::WordForward),
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.buffer().rope().to_string(), "two three");

        // Neither of these is a change.
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Operate {
            op: Operator::Yank,
            target: Target::Motion(Motion::WordForward),
            count: 1,
            sink: Sink::Ring,
        }));

        // `dw` again, from where the motion left the cursor — verified against
        // vim, which gives the same.
        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "tthree", "still the delete");
    }

    #[test]
    fn undo_does_not_become_the_thing_dot_repeats() {
        let mut ed = editor("abcdef");
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Right),
            count: 1,
            sink: Sink::Ring,
        }));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().rope().to_string(), "abcdef");
        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "bcdef", "the delete, not the undo");
    }

    #[test]
    fn dot_carries_the_original_count() {
        let mut ed = editor("abcdefghi");
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Right),
            count: 3,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.buffer().rope().to_string(), "defghi");
        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "ghi", "three more");
    }

    #[test]
    fn a_count_on_the_dot_itself_replaces_the_original() {
        let mut ed = editor("abcdef");
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Right),
            count: 1,
            sink: Sink::Ring,
        }));
        ed.apply(cmd(Action::RepeatChange { count: Some(3) }));
        assert_eq!(ed.buffer().rope().to_string(), "ef");
    }

    #[test]
    fn dot_with_nothing_recorded_says_so() {
        let mut ed = editor("abc");
        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "abc");
        assert!(!ed.session.status.is_empty());
    }

    /// A visual operator has no selection left by the time `.` runs, so it
    /// repeats over the same extent from wherever the cursor is.
    #[test]
    fn dot_after_a_charwise_visual_delete_repeats_the_extent() {
        let mut ed = editor("abcdefgh");
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "defgh");

        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "gh", "three more characters");
    }

    #[test]
    fn dot_after_a_linewise_visual_delete_repeats_the_line_count() {
        let mut ed = editor("1\n2\n3\n4\n5");
        ed.apply(cmd(Action::EnterVisual(VisualKind::Line)));
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().rope().to_string(), "3\n4\n5");

        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "5", "two more lines");
    }

    #[test]
    fn a_replay_does_not_record_itself() {
        // Otherwise the second `.` would repeat the repeat and counts compound.
        let mut ed = editor("aaaaaaaa");
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Right),
            count: 2,
            sink: Sink::Ring,
        }));
        dot(&mut ed);
        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "aa", "two at a time, three times");
    }

    #[test]
    fn dot_runs_at_every_cursor() {
        let mut ed = with_cursors("ax ax ax", &[0, 3, 6]);
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Right),
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.buffer().rope().to_string(), "x x x");
        dot(&mut ed);
        assert_eq!(ed.buffer().rope().to_string(), "  ");
    }

    // ---- pre-existing bugs `.` uncovered ----------------------------------

    /// Vim steps the cursor one left when leaving insert, back onto the last
    /// character typed. Only clamping it left the cursor one column too far
    /// right, which `.` then made visible.
    #[test]
    fn leaving_insert_steps_the_cursor_back_onto_what_was_typed() {
        let mut ed = editor("hello");
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "AB");
        assert_eq!(ed.cursor_col(), 2, "past the B while inserting");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.cursor_col(), 1, "back onto the B");
    }

    #[test]
    fn leaving_visual_mode_does_not_step_the_cursor() {
        let mut ed = editor("hello");
        ed.set_cursor(Cursor::at(3));
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.cursor_col(), 3, "only insert steps back");
    }

    // ---- search ------------------------------------------------------------

    fn search_for(ed: &mut Editor, pattern: &str, forward: bool) {
        ed.apply(cmd(Action::EnterSearch { forward, operator: None, count: 1 }));
        for c in pattern.chars() {
            ed.apply(cmd(Action::SearchChar(c)));
        }
        ed.apply(cmd(Action::SearchExecute));
    }

    #[test]
    fn a_search_lands_on_the_first_character_of_the_match() {
        let mut ed = editor("one two three");
        search_for(&mut ed, "three", true);
        assert_eq!(ed.cursor().at, 8);
    }

    #[test]
    fn a_search_wraps_round_the_end() {
        let mut ed = editor("one two");
        ed.set_cursor(Cursor::at(6));
        search_for(&mut ed, "one", true);
        assert_eq!(ed.cursor().at, 0);
    }

    #[test]
    fn a_backward_search_goes_the_other_way() {
        let mut ed = editor("one two three");
        ed.set_cursor(Cursor::at(12));
        search_for(&mut ed, "two", false);
        assert_eq!(ed.cursor().at, 4);
    }

    #[test]
    fn n_repeats_in_the_direction_the_search_was_typed() {
        let mut ed = editor("a1a2a3");
        ed.set_cursor(Cursor::at(5));
        search_for(&mut ed, "a", false);
        assert_eq!(ed.cursor().at, 4, "backward to the third a");
        ed.apply(cmd(Action::Move(Motion::Search { reverse: false })));
        assert_eq!(ed.cursor().at, 2, "n keeps going backward");
        ed.apply(cmd(Action::Move(Motion::Search { reverse: true })));
        assert_eq!(ed.cursor().at, 4, "N reverses");
    }

    /// A search is a motion, which is most of why it is worth having.
    #[test]
    fn a_search_is_an_operator_target_and_is_exclusive() {
        let mut ed = editor("one two three four");
        ed.apply(cmd(Action::EnterSearch {
            forward: true,
            operator: Some((Operator::Delete, Sink::Ring)),
            count: 1,
        }));
        for c in "three".chars() {
            ed.apply(cmd(Action::SearchChar(c)));
        }
        ed.apply(cmd(Action::SearchExecute));
        assert_eq!(ed.buffer().rope().to_string(), "three four", "stops before the match");
    }

    #[test]
    fn smartcase_is_insensitive_until_the_pattern_has_a_capital() {
        // A search finds the *next* match, so both of these start before the
        // only candidate rather than on it.
        let mut ed = editor("bar FOO");
        search_for(&mut ed, "foo", true);
        assert_eq!(ed.cursor().at, 4, "an all-lowercase pattern ignores case");

        // Two candidates, differing only in case: a capital in the pattern
        // makes it skip the lowercase one.
        let mut ed = editor("x foo Foo");
        search_for(&mut ed, "Foo", true);
        assert_eq!(ed.cursor().at, 6, "a capital makes it case-sensitive");
    }

    #[test]
    fn star_matches_whole_words_only() {
        let mut ed = editor("foo\nfoobar\nfoo");
        ed.apply(cmd(Action::SearchWord { forward: true }));
        assert_eq!(ed.cursor_row(), 2, "skipped foobar");
    }

    #[test]
    fn a_pattern_that_is_not_there_reports_and_does_not_move() {
        let mut ed = editor("abc");
        ed.set_cursor(Cursor::at(1));
        search_for(&mut ed, "zzz", true);
        assert_eq!(ed.cursor().at, 1);
        assert!(ed.session.status.contains("not found"), "got: {}", ed.session.status);
    }

    #[test]
    fn a_bare_search_repeats_the_last_pattern() {
        let mut ed = editor("a1a2a3");
        search_for(&mut ed, "a", true);
        assert_eq!(ed.cursor().at, 2);
        search_for(&mut ed, "", true);
        assert_eq!(ed.cursor().at, 4, "the empty pattern reuses the last one");
    }

    #[test]
    fn cancelling_the_search_line_leaves_everything_alone() {
        let mut ed = editor("one two");
        ed.apply(cmd(Action::EnterSearch {
            forward: true,
            operator: Some((Operator::Delete, Sink::Ring)),
            count: 1,
        }));
        ed.apply(cmd(Action::SearchChar('t')));
        ed.apply(cmd(Action::SearchCancel));
        assert_eq!(ed.session.mode, Mode::Normal);
        assert_eq!(ed.buffer().rope().to_string(), "one two", "the operator went with it");
    }

    #[test]
    fn backspacing_off_the_front_of_the_search_line_cancels() {
        let mut ed = editor("abc");
        ed.apply(cmd(Action::EnterSearch { forward: true, operator: None, count: 1 }));
        ed.apply(cmd(Action::SearchChar('a')));
        ed.apply(cmd(Action::SearchBackspace));
        ed.apply(cmd(Action::SearchBackspace));
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn searching_leaves_highlighting_alone_and_hls_asks_for_it() {
        let mut ed = editor("foo foo");
        search_for(&mut ed, "foo", true);
        assert!(
            !ed.session.highlight_search,
            "a plain `/` does not light the buffer up, as in vim"
        );

        ed.run_ex("hls");
        assert!(ed.session.highlight_search);
        ed.run_ex("noh");
        assert!(!ed.session.highlight_search);
    }

    #[test]
    fn the_status_line_echoes_the_search_with_the_direction_it_is_going() {
        let mut ed = editor("a1a2a3");
        search_for(&mut ed, "a", true);
        assert_eq!(ed.session.status, "/a");

        ed.apply(cmd(Action::Move(Motion::Search { reverse: false })));
        assert_eq!(ed.session.status, "/a", "`n` keeps going forward");

        ed.apply(cmd(Action::Move(Motion::Search { reverse: true })));
        assert_eq!(ed.session.status, "?a", "`N` is the same pattern the other way");
    }

    #[test]
    fn the_search_keeps_the_status_line_while_the_keys_are_still_the_search() {
        let mut ed = editor("a1a2a3");
        search_for(&mut ed, "a", true);
        assert!(ed.session.search_focus, "the line is the search's from the moment it runs");

        ed.apply(cmd(Action::Move(Motion::Search { reverse: false })));
        assert!(ed.session.search_focus, "`n` is still the search");
        ed.apply(cmd(Action::Move(Motion::Search { reverse: true })));
        assert!(ed.session.search_focus, "and so is `N`");

        ed.apply(cmd(Action::Move(Motion::Right)));
        assert!(!ed.session.search_focus, "anything else hands the line back");
    }

    #[test]
    fn abandoning_the_search_line_hands_the_status_line_back() {
        let mut ed = editor("a1a2a3");
        ed.apply(cmd(Action::EnterSearch { forward: true, operator: None, count: 1 }));
        ed.apply(cmd(Action::SearchChar('a')));
        assert!(ed.session.search_focus);

        ed.apply(cmd(Action::SearchCancel));
        assert!(!ed.session.search_focus, "`Esc` abandons the search, so it abandons the line");
    }

    #[test]
    fn star_takes_the_status_line_too() {
        let mut ed = editor("foo bar foo");
        ed.apply(cmd(Action::SearchWord { forward: true }));
        assert!(ed.session.search_focus);
        assert_eq!(ed.session.status, "/foo");
    }

    #[test]
    fn the_match_count_says_which_one_the_cursor_is_on() {
        let mut ed = editor("a1a2a3");
        search_for(&mut ed, "a", true);
        assert_eq!(ed.search_count(), Some((2, 3)), "the search moved onto the second");

        ed.apply(cmd(Action::Move(Motion::Search { reverse: false })));
        assert_eq!(ed.search_count(), Some((3, 3)));

        ed.set_cursor(Cursor::at(1));
        assert_eq!(ed.search_count(), Some((1, 3)), "off a match, it counts what is behind");
    }

    #[test]
    fn the_match_count_follows_an_edit() {
        let mut ed = editor("a1a2a3");
        search_for(&mut ed, "a", true);
        assert_eq!(ed.search_count(), Some((2, 3)));

        ed.set_cursor(Cursor::at(0));
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Right),
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.search_count(), Some((0, 2)), "the cache is keyed on the edit count");
    }

    #[test]
    fn there_is_no_count_before_anything_has_been_searched_for() {
        let mut ed = editor("foo");
        assert_eq!(ed.search_count(), None);
    }

    #[test]
    fn a_search_delete_is_repeatable_with_dot() {
        let mut ed = editor("aXbXc");
        ed.apply(cmd(Action::EnterSearch {
            forward: true,
            operator: Some((Operator::Delete, Sink::Ring)),
            count: 1,
        }));
        ed.apply(cmd(Action::SearchChar('X')));
        ed.apply(cmd(Action::SearchExecute));
        assert_eq!(ed.buffer().rope().to_string(), "XbXc");
        ed.apply(cmd(Action::RepeatChange { count: None }));
        assert_eq!(ed.buffer().rope().to_string(), "Xc");
    }

    // ---- scrolling ---------------------------------------------------------

    fn tall(lines: usize) -> Editor {
        let text: String = (1..=lines).map(|i| format!("line{i:02}\n")).collect();
        let mut ed = editor(&text);
        ed.set_cursor(Cursor::at(0));
        ed.scroll_to_cursor(9);
        ed
    }

    #[test]
    fn ctrl_e_moves_the_window_and_pushes_the_cursor_out_of_the_margin() {
        let mut ed = tall(30);
        assert_eq!(ed.scroll(), 0);
        ed.apply(cmd(Action::ScrollLine { down: true }));
        assert_eq!(ed.scroll(), 1, "the window moved one line");
        assert_eq!(ed.cursor_row(), 4, "and the cursor was pushed clear of the scrolloff");
    }

    #[test]
    fn ctrl_y_moves_the_window_back() {
        let mut ed = tall(30);
        for _ in 0..5 {
            ed.apply(cmd(Action::ScrollLine { down: true }));
        }
        assert_eq!(ed.scroll(), 5);
        ed.apply(cmd(Action::ScrollLine { down: false }));
        assert_eq!(ed.scroll(), 4);
    }

    #[test]
    fn ctrl_d_moves_half_a_window_and_takes_the_cursor_with_it() {
        let mut ed = tall(30);
        ed.apply(cmd(Action::ScrollHalfPage { down: true }));
        assert_eq!(ed.scroll(), 4, "half of nine, rounded down");
        // The cursor would keep its place in the window at row 4, but that is
        // the very top of the new window and scrolloff pushes it clear. Vim
        // with `scrolloff=3` lands in the same place — checked through a pty.
        assert_eq!(ed.cursor_row(), 7);
    }

    #[test]
    fn ctrl_u_comes_back() {
        let mut ed = tall(30);
        ed.apply(cmd(Action::ScrollHalfPage { down: true }));
        ed.apply(cmd(Action::ScrollHalfPage { down: true }));
        assert_eq!(ed.scroll(), 8);
        ed.apply(cmd(Action::ScrollHalfPage { down: false }));
        assert_eq!(ed.scroll(), 4);
    }

    #[test]
    fn scrolling_stops_at_the_ends_rather_than_running_off() {
        let mut ed = tall(30);
        for _ in 0..50 {
            ed.apply(cmd(Action::ScrollHalfPage { down: true }));
        }
        assert_eq!(ed.scroll(), 30 - 9, "the last line stays on screen");
        for _ in 0..50 {
            ed.apply(cmd(Action::ScrollHalfPage { down: false }));
        }
        assert_eq!(ed.scroll(), 0);
    }

    #[test]
    fn a_file_shorter_than_the_window_does_not_scroll() {
        let mut ed = tall(3);
        ed.apply(cmd(Action::ScrollHalfPage { down: true }));
        assert_eq!(ed.scroll(), 0);
    }
}
