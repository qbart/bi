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
use crate::tree::{ClipMode, Clipboard, Kind, Tree, copy_into, move_into};
use crate::window::{
    Chrome, Content, ContentKind, Dir, Layout, Rect, Side, Text, Window, WindowId,
};

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
/// See `docs/specs/number.md`. The rules live here rather than in the
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

    /// Changes which buffer the focused window shows, or the list itself.
    ///
    /// Handled by `Editor` before a `View` is built, because a view borrows
    /// from the very list these change.
    Buffer(BufferCmd),
    /// Splits, closes, resizes or switches windows. Handled beside
    /// [`Action::Buffer`] and for the same reason.
    Window(WindowCmd),
    /// A key in a window holding a tree. Handled beside the two above, and for
    /// the same reason: it changes what a window shows.
    Tree(TreeCmd),
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

/// A paste stopped on a name it cannot use.
///
/// The cost of never overwriting anything: a paste can be half-done, and what
/// is left has to wait somewhere while the command line asks for a name.
#[derive(Debug, Clone)]
pub struct Pasting {
    /// Still to place, the head first.
    queue: Vec<std::path::PathBuf>,
    into: std::path::PathBuf,
    mode: ClipMode,
    /// How many landed before it stopped, for the message when it is abandoned.
    done: usize,
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
    /// Paths marked in the tree for the next paste. Beside the registers, and
    /// session state for the same reason: you mark in one place and put them
    /// in another. See `docs/specs/tree.md`.
    pub clipboard: Clipboard,
    /// A paste waiting on a name for the file it stopped at.
    pub pasting: Option<Pasting>,
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
    /// Session-wide by choice, where vim scopes `'number'` per window.
    ///
    /// A gutter that is numbered in one pane and not in its neighbour makes the
    /// same file read differently depending on where you opened it, and the
    /// setting is a reading preference rather than anything about the view. One
    /// value, and every window obeys it. Not a placeholder for a per-window
    /// option later — see `docs/specs/windows.md`.
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
    /// Where the last window to leave this buffer was looking.
    ///
    /// Without it, cycling forward and back through three files loses your
    /// place in all of them, which makes buffer cycling something you use
    /// once. When two windows show one buffer, the last to leave is what this
    /// remembers — there is no better answer, and it costs nothing to say
    /// which one wins.
    last: Cursors,
}

/// A parsed `:` line.
///
/// The split that matters is which arms need a rope: those are the last four,
/// and they are the only ones that cannot run in a window holding a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExLine {
    Window(WindowCmd),
    Buffer(BufferCmd),
    /// `:e <path>`. Bare `:e` is [`ExLine::Reload`], which is a different job.
    Edit {
        path: String,
    },
    Quit {
        force: bool,
    },
    /// `:qa` — every buffer has to agree, not just this one.
    QuitAll {
        force: bool,
    },
    /// `:wa` — writes every modified buffer.
    WriteAll,
    Highlight(bool),
    Set(String),
    Create(String),
    Rename {
        from: String,
        to: String,
    },
    Delete {
        path: String,
        force: bool,
    },
    /// `:paste [<dir>]` — what is marked, into `<dir>` or the selected one.
    Paste(Option<String>),
    /// `:paste-as <path>` — place the one that stopped, and carry on.
    PasteAs(String),
    Unknown(String),
    /// Parsed, but cannot run — carrying its own message, already phrased.
    Error(String),

    Write(String),
    WriteQuit(String),
    /// Bare `:e` — re-read this file from disk.
    Reload {
        force: bool,
    },
    /// `:42`.
    Goto(usize),
}

/// Splits a `:` line into a command and its argument. `None` for a blank line,
/// which is not an error and not a command.
fn parse_ex(line: &str) -> Option<ExLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let (cmd, arg) = match line.split_once(char::is_whitespace) {
        Some((c, a)) => (c, a.trim()),
        None => (line, ""),
    };
    let force = cmd.ends_with('!');
    let name = cmd.trim_end_matches('!');
    let split = |dir| {
        ExLine::Window(WindowCmd::Split { dir, path: (!arg.is_empty()).then(|| arg.to_string()) })
    };

    Some(match name {
        "w" | "write" => ExLine::Write(arg.into()),
        // The `a` forms mean "every buffer", which they genuinely do.
        "wa" | "wall" => ExLine::WriteAll,
        "qa" | "qall" => ExLine::QuitAll { force },
        "q" | "quit" => ExLine::Quit { force },
        "sp" | "split" | "new" => split(Dir::Horizontal),
        "vs" | "vsp" | "vsplit" | "vnew" => split(Dir::Vertical),
        "clo" | "close" => ExLine::Window(WindowCmd::Close),
        "on" | "only" => ExLine::Window(WindowCmd::Only),
        // Bare `:e` reloads this buffer; with a path it changes what the
        // window shows, which is two different jobs under one name.
        "e" | "edit" if arg.is_empty() => ExLine::Reload { force },
        "e" | "edit" => ExLine::Edit { path: arg.into() },
        "bn" | "bnext" => ExLine::Buffer(BufferCmd::Next),
        "bp" | "bprev" | "bprevious" => ExLine::Buffer(BufferCmd::Prev),
        "bd" | "bdelete" => ExLine::Buffer(BufferCmd::Delete { force }),
        "ls" | "buffers" => ExLine::Buffer(BufferCmd::List),
        // `:b#` written without a space, which is how it is spelt everywhere
        // — including in this project's own README until now.
        "b#" | "buffer#" => ExLine::Buffer(BufferCmd::Alternate),
        "b" | "buffer" => match arg {
            "" => ExLine::Error("which buffer?".into()),
            "#" => ExLine::Buffer(BufferCmd::Alternate),
            partial => ExLine::Buffer(BufferCmd::Named(partial.into())),
        },
        "noh" | "nohl" | "nohlsearch" => ExLine::Highlight(false),
        // Off by default, because a plain `/` in vim does not light up the
        // buffer. The count in the status line is what a search owes you.
        "hls" | "hlsearch" => ExLine::Highlight(true),
        "set" => ExLine::Set(arg.into()),
        // The tree keys are prefills over these three, which is why they are
        // ex commands rather than tree-only actions: typeable without a tree,
        // and testable without one.
        "create" if !arg.is_empty() => ExLine::Create(arg.into()),
        "create" => ExLine::Error("create what?".into()),
        "rename" => match arg.split_once(char::is_whitespace) {
            Some((from, to)) if !to.trim().is_empty() => {
                ExLine::Rename { from: from.into(), to: to.trim().into() }
            }
            _ => ExLine::Error("rename takes the old path and the new one".into()),
        },
        "delete" if !arg.is_empty() => ExLine::Delete { path: arg.into(), force },
        "delete" => ExLine::Error("delete what?".into()),
        "paste" => ExLine::Paste((!arg.is_empty()).then(|| arg.to_string())),
        "paste-as" if !arg.is_empty() => ExLine::PasteAs(arg.into()),
        "paste-as" => ExLine::Error("paste it as what?".into()),
        "wq" | "x" => ExLine::WriteQuit(arg.into()),
        _ => match name.parse::<usize>() {
            Ok(row) => ExLine::Goto(row),
            Err(_) => ExLine::Unknown(name.into()),
        },
    })
}

/// What `Ctrl-W` and the split commands do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowCmd {
    /// A bare split duplicates the window — same buffer, same cursor, same
    /// scroll — so the new pane lands on the line you were reading.
    Split {
        dir: Dir,
        path: Option<String>,
    },
    /// `Ctrl-W e` — a tree beside this window, rooted at the file it is
    /// showing, with that file selected.
    Tree,
    Focus(Side),
    Cycle {
        back: bool,
    },
    Close,
    Only,
    /// `Ctrl-W + - < >`, in cells along `axis`.
    Resize {
        axis: Dir,
        cells: i32,
    },
    Equalize,
}

/// What a key does in a window holding a tree.
///
/// Every arm here the tree can answer by itself; `Expand` and `Enter` are the
/// two that may instead reach past it and open a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeCmd {
    /// `j` / `k`, with their count.
    Select {
        down: bool,
        count: usize,
    },
    First,
    Last,
    HalfPage {
        down: bool,
    },
    /// `l` — open a directory, or open the file under the cursor.
    Expand,
    /// `h` — close a directory, or step to the parent row.
    Collapse,
    /// `Enter` — a directory toggles, a file opens.
    Enter,
    /// `-` — re-root at the parent directory.
    Up,
    /// `+` — re-root at the selected directory, the inverse of `-`.
    Down,
    /// `dd` — delete the selected path outright, with no `:` line in between.
    Delete,
    /// `y` — the selected path into the register ring.
    Yank,
    /// `c` / `x` — mark for the next paste, or take the mark off.
    Mark(ClipMode),
    /// `p` — put what is marked into the selected directory.
    Paste,
    /// `Esc` — forget what is marked.
    ClearMarks,
    Refresh,
    ToggleHidden,
    /// `a` `r` `d` — fills the command line in and hands over.
    Prompt(FileOp),
}

/// The three things a tree key can start.
///
/// Each one only prefills a `:` line: the commands themselves are ordinary ex
/// commands, typeable and testable with no tree open at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOp {
    Create,
    Rename,
}

/// Which buffer a window should show, and what to do to the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferCmd {
    Next,
    Prev,
    /// `Ctrl-^` / `:b#`.
    Alternate,
    /// `:b <partial>` — matched against the path.
    Named(String),
    /// `:ls` — the picker over the list.
    List,
    /// A row accepted from that picker, as a position in the list.
    Chosen(usize),
    Delete {
        force: bool,
    },
}

/// The session: every open buffer, every window onto them, and the state that
/// belongs to neither.
pub struct Editor {
    buffers: Vec<BufferEntry>,
    windows: Vec<Window>,
    layout: Layout,
    focus: WindowId,
    /// The window that was focused before this one.
    ///
    /// Where a file opened from a tree goes, which is what makes `:vs .` a
    /// sidebar without anything here being one. `Ctrl-W p` would read the same
    /// field.
    previous: Option<WindowId>,
    next_buffer: u32,
    next_window: u32,
    /// The area and chrome the frontend last laid out in.
    ///
    /// Splitting, resizing and directional switching are all geometry
    /// questions, and geometry needs a size. Before the first frame there is
    /// none, so those commands act on a zero area and say they had no room —
    /// which is true.
    area: Rect,
    chrome: Chrome,
    pub session: Session,
}

/// One window and what it shows, borrowed to be drawn.
pub enum Pane<'a> {
    Text { window: &'a Window, text: &'a Text, buffer: &'a Buffer, syntax: Option<&'a Syntax> },
    Tree { window: &'a Window, tree: &'a Tree },
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
    pub selections: &'a mut Selections,
    pub scroll: &'a mut usize,
    /// Which window this is, for the commands that name one.
    ///
    /// The window itself cannot come too — borrowing it whole and borrowing the
    /// selections inside it are the same borrow. So the geometry is unpacked
    /// beside them: `height` mutably, because `scroll_to_cursor` records the
    /// room it was given for `Ctrl-D` to halve later, and `width` by value,
    /// because only the frontend sets that.
    pub window: WindowId,
    pub height: &'a mut usize,
    pub width: usize,
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

    /// A directory opens a tree, and leaves a `[No Name]` in the list behind
    /// it — the list is never empty, so nothing downstream has to learn that
    /// the session started on a directory.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.is_dir() {
            let mut editor = Self::empty();
            // Assigned rather than shown: there was nothing here before, so
            // there is no alternate to remember.
            editor.window_mut().content = Content::Tree(Tree::new(path)?);
            return Ok(editor);
        }
        Ok(Self::with_buffer(Buffer::open(path)?))
    }

    fn with_buffer(buffer: Buffer) -> Self {
        let (buffer_id, window_id) = (BufferId(0), WindowId(0));
        Self {
            buffers: vec![BufferEntry {
                id: buffer_id,
                syntax: syntax_for(&buffer),
                buffer,
                last: Vec::new(),
            }],
            windows: vec![Window::new(window_id, buffer_id)],
            layout: Layout::new(window_id),
            focus: window_id,
            previous: None,
            next_buffer: 1,
            next_window: 1,
            area: Rect::default(),
            chrome: Chrome::default(),
            session: Session::default(),
        }
    }

    // ---- what the focused view is -------------------------------------------

    /// Which keymap the focused window wants. The frontend passes this back
    /// into `Input::on_key`.
    pub fn content_kind(&self) -> ContentKind {
        self.window().content.kind()
    }

    /// What a given window holds, for a frontend deciding how to draw it.
    pub fn content_kind_of(&self, id: WindowId) -> Option<ContentKind> {
        Some(self.window_of(id)?.content.kind())
    }

    /// The window holding the tree, if one is open.
    ///
    /// There is at most one: `Ctrl-W e` toggles it rather than opening a
    /// second, and `-` goes to the one that exists. A tree is a place you look
    /// things up, and two of them is two of the same thing.
    pub fn tree_window(&self) -> Option<WindowId> {
        self.window_ids()
            .into_iter()
            .find(|id| self.window_of(*id).is_some_and(|w| w.tree().is_some()))
    }

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

    /// The focused window's buffer, when it is showing one.
    ///
    /// `None` for a tree pane. The buffer list is still never empty — what
    /// stopped being true when trees arrived is that every *window* names
    /// something in it. See `docs/specs/tree.md`.
    pub fn buffer(&self) -> Option<&Buffer> {
        Some(&self.entry(self.window().buffer()?).buffer)
    }

    pub fn buffer_mut(&mut self) -> Option<&mut Buffer> {
        let id = self.window().buffer()?;
        Some(&mut self.entry_mut(id).buffer)
    }

    pub fn syntax(&self) -> Option<&Syntax> {
        self.entry(self.window().buffer()?).syntax.as_ref()
    }

    pub fn selections(&self) -> Option<&Selections> {
        Some(&self.window().text()?.selections)
    }

    /// Everything needed to *draw* one window, borrowed immutably.
    ///
    /// A renderer draws windows it is not editing, so it cannot go through
    /// `View` — that would mean borrowing the session mutably to read a rope.
    pub fn pane(&self, id: WindowId) -> Option<Pane<'_>> {
        let window = self.window_of(id)?;
        Some(match &window.content {
            Content::Tree(tree) => Pane::Tree { window, tree },
            Content::Text(text) => {
                let entry = self.entry(text.buffer);
                Pane::Text { window, text, buffer: &entry.buffer, syntax: entry.syntax.as_ref() }
            }
        })
    }

    /// The buffer a given window shows, if it shows one.
    pub fn buffer_of(&self, id: WindowId) -> Option<&Buffer> {
        Some(&self.entry(self.window_of(id)?.buffer()?).buffer)
    }

    /// The block's span on one row of a given window.
    pub fn block_span_in(&self, id: WindowId, row: usize) -> (usize, usize) {
        let (Some(buffer), Some(text)) =
            (self.buffer_of(id), self.window_of(id).and_then(Window::text))
        else {
            return (0, 0);
        };
        span_of_block_at(buffer, &text.selections, self.session.block_to_eol, row)
    }

    /// First visible row of the focused window.
    pub fn scroll(&self) -> usize {
        self.window().text().map_or(0, |text| text.scroll)
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
    /// `None` when that window holds a tree, which is the compiler-checked
    /// form of "the editing commands never see one".
    pub fn view(&mut self, id: WindowId) -> Option<View<'_>> {
        let window = self.windows.iter_mut().find(|w| w.id == id)?;
        let (window_id, width) = (window.id, window.width);
        // Destructured rather than borrowed whole: `height` and the selections
        // live in different fields, and only splitting them lets the view hold
        // both at once.
        let Window { content, height, .. } = window;
        let Content::Text(text) = content else { return None };
        let entry = self
            .buffers
            .iter_mut()
            .find(|b| b.id == text.buffer)
            .expect("a window's buffer is always in the list");
        Some(View {
            id: entry.id,
            buffer: &mut entry.buffer,
            syntax: &mut entry.syntax,
            selections: &mut text.selections,
            scroll: &mut text.scroll,
            window: window_id,
            height,
            width,
            session: &mut self.session,
        })
    }

    pub fn focused(&mut self) -> Option<View<'_>> {
        self.view(self.focus)
    }

    // ---- the buffer list ----------------------------------------------------

    /// Every open buffer, in the order they were opened.
    ///
    /// That order is vim's buffer numbering without the numbers: what `:bn`
    /// walks and what the picker lists.
    pub fn buffer_ids(&self) -> Vec<BufferId> {
        self.buffers.iter().map(|b| b.id).collect()
    }

    /// A buffer's name for the status line and the picker.
    pub fn name_of(&self, id: BufferId) -> String {
        self.buffers
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| b.buffer.path.as_ref())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[No Name]".into())
    }

    pub fn is_modified(&self, id: BufferId) -> bool {
        self.buffers.iter().find(|b| b.id == id).is_some_and(|b| b.buffer.is_modified())
    }

    fn fresh_buffer_id(&mut self) -> BufferId {
        let id = BufferId(self.next_buffer);
        self.next_buffer += 1;
        id
    }

    /// The buffer for `path`, opening the file only if it is not already open.
    ///
    /// Reuse is not an optimisation: two live ropes over one path would let a
    /// window edit text another window cannot see, and one of them would win
    /// on the next `:w`.
    fn open_path(&mut self, path: &str) -> Result<BufferId> {
        let wanted = std::path::Path::new(path);
        if let Some(entry) = self.buffers.iter().find(|b| b.buffer.path.as_deref() == Some(wanted))
        {
            return Ok(entry.id);
        }
        let buffer = Buffer::open(path)?;
        let id = self.fresh_buffer_id();
        self.buffers.push(BufferEntry {
            id,
            syntax: syntax_for(&buffer),
            buffer,
            last: Vec::new(),
        });
        Ok(id)
    }

    /// Points a window at another buffer, saving where it was and restoring
    /// where it last was in the one it is entering.
    fn show(&mut self, window: WindowId, to: BufferId) {
        let Some(current) = self.window_of(window) else { return };
        let from = current.buffer();
        if from == Some(to) {
            return;
        }

        // Leaving a buffer writes where this window was into the entry, whether
        // what replaces it is another buffer or a tree.
        if let Some(from) = from {
            let leaving = current.text().map(|text| text.selections.as_pairs()).unwrap_or_default();
            self.entry_mut(from).last = leaving;
        }

        // Clamped, because the file may have been edited from another window
        // since this one last looked at it — and unlike a live window, there
        // was nothing here to shift through those edits.
        let len = self.entry(to).buffer.rope().len_chars();
        let last: Cursors = self
            .entry(to)
            .last
            .iter()
            .map(|&(anchor, head)| (anchor.min(len), head.min(len)))
            .collect();

        let mut text = Text::new(to);
        if !last.is_empty() {
            text.selections = Selections::from_pairs(last);
        }
        let window = self.window_mut_of(window).expect("checked above");
        window.show(Content::Text(text));
    }

    fn run_buffer_cmd(&mut self, cmd: BufferCmd) {
        let focus = self.focus;

        // A parked tree is an alternate like any other, and swapping back is
        // the whole reason displacing it is tolerable: re-reading the
        // directory would lose the expansion you had built up.
        // Checked before taking: a buffer alternate must stay where it is, and
        // `take` on a failed pattern would still have emptied the slot.
        if cmd == BufferCmd::Alternate && matches!(self.window().alt, Some(Content::Tree(_))) {
            let tree = self.window_mut().alt.take().expect("checked above");
            return self.window_mut().show(tree);
        }

        let ids = self.buffer_ids();
        let current = self.window().buffer();
        let at = current.and_then(|id| ids.iter().position(|&i| i == id)).unwrap_or(0);

        let target = match cmd {
            BufferCmd::Next => Some(ids[(at + 1) % ids.len()]),
            BufferCmd::Prev => Some(ids[(at + ids.len() - 1) % ids.len()]),
            BufferCmd::Alternate => match self.window().alt_buffer() {
                Some(alt) if ids.contains(&alt) => Some(alt),
                _ => {
                    self.session.status = "no alternate buffer".into();
                    None
                }
            },
            BufferCmd::Chosen(i) => ids.get(i).copied(),
            BufferCmd::Named(ref partial) => {
                let hits: Vec<BufferId> = ids
                    .iter()
                    .copied()
                    .filter(|&id| self.name_of(id).contains(partial.as_str()))
                    .collect();
                match hits.len() {
                    1 => Some(hits[0]),
                    0 => {
                        self.session.status = format!("no buffer matching \"{partial}\"");
                        None
                    }
                    // Names them rather than guessing: picking one of several
                    // would silently open the wrong file.
                    _ => {
                        let names: Vec<String> = hits.iter().map(|&id| self.name_of(id)).collect();
                        self.session.status =
                            format!("more than one buffer matches: {}", names.join(", "));
                        None
                    }
                }
            }
            BufferCmd::List => {
                self.open_buffer_picker();
                None
            }
            BufferCmd::Delete { force } => {
                match current {
                    Some(id) => self.delete_buffer(id, force),
                    // A tree pane shows no buffer, so there is nothing here to
                    // delete — unlike `:bn`, which is a request to show one.
                    None => self.session.status = "no buffer in this window".into(),
                }
                None
            }
        };

        if let Some(target) = target {
            self.show(focus, target);
            self.session.status = self.name_of(target);
        }
    }

    /// Removes a buffer from the list, leaving every window open.
    ///
    /// Deleting a file should not rearrange the screen: a window showing the
    /// deleted buffer falls to the next one in the list instead of closing.
    fn delete_buffer(&mut self, id: BufferId, force: bool) {
        if self.is_modified(id) && !force {
            self.session.status = "unsaved changes (use `:bd!` to discard)".into();
            return;
        }

        let ids = self.buffer_ids();
        let at = ids.iter().position(|&b| b == id).unwrap_or(0);
        let name = self.name_of(id);

        // The list is never empty: deleting the last buffer leaves a fresh one
        // in its place, so no path has to handle a session with nothing open.
        // (A window may still show no buffer — that is a tree, not an empty
        // list.)
        let heir = if ids.len() == 1 {
            let fresh = self.fresh_buffer_id();
            let buffer = Buffer::empty();
            self.buffers.push(BufferEntry {
                id: fresh,
                syntax: syntax_for(&buffer),
                buffer,
                last: Vec::new(),
            });
            fresh
        } else {
            ids[(at + 1) % ids.len()]
        };

        for window in 0..self.windows.len() {
            if self.windows[window].buffer() == Some(id) {
                let w = self.windows[window].id;
                self.show(w, heir);
            }
            // A stable id that resolves to nothing is the one way it is worse
            // than an index, which at least fails loudly.
            if self.windows[window].alt_buffer() == Some(id) {
                self.windows[window].alt = None;
            }
        }

        self.buffers.retain(|b| b.id != id);
        self.session.status = format!("\"{name}\" deleted");
    }

    fn open_buffer_picker(&mut self) {
        let items = self
            .buffer_ids()
            .into_iter()
            .map(|id| Item { text: self.name_of(id), badge: self.is_modified(id).then_some('+') })
            .collect();
        self.session.picker = Some(Picker::new(PickerKind::Buffer, items));
        self.session.mode = Mode::Pick;
    }

    // ---- the window tree ----------------------------------------------------

    /// Every window, in layout order.
    pub fn window_ids(&self) -> Vec<WindowId> {
        self.layout.leaves()
    }

    /// Puts the tree away.
    ///
    /// Closing the window, unless it is the last one — that one can never
    /// close, so it shows a buffer instead. Which is what Enter on a file does
    /// from a `bee .` session, and leaves a session that still has a window.
    fn close_tree(&mut self, id: WindowId) {
        if self.windows.len() > 1 {
            self.close_window(id);
            return;
        }
        match self.window_mut_of(id).and_then(|w| w.alt.take()) {
            Some(alt) => self.window_mut_of(id).expect("checked above").content = alt,
            None => {
                let first = self.buffer_ids()[0];
                if let Some(window) = self.window_mut_of(id) {
                    window.content = Content::Text(Text::new(first));
                }
            }
        }
    }

    /// Moves focus, remembering where it came from.
    ///
    /// Every focus change goes through here, so `previous` cannot drift out of
    /// step with the one field it exists to shadow.
    fn set_focus(&mut self, id: WindowId) {
        if id == self.focus {
            return;
        }
        self.previous = Some(self.focus);
        self.focus = id;
    }

    /// Where a file opened from a tree should go.
    ///
    /// The last window focused before this one, when it still exists and still
    /// holds text; failing that the first text window in layout order; failing
    /// that this one, which is the single-window case and means the tree is
    /// displaced. See `docs/specs/tree.md`.
    fn handoff_window(&self) -> WindowId {
        let usable = |id: &WindowId| {
            *id != self.focus && self.window_of(*id).is_some_and(|w| w.text().is_some())
        };
        self.previous
            .filter(usable)
            .or_else(|| self.window_ids().into_iter().find(|id| usable(id)))
            .unwrap_or(self.focus)
    }

    fn run_tree_cmd(&mut self, cmd: TreeCmd) {
        let height = self.window().height;
        if self.window().tree().is_none() {
            // `-` is the same key in the other direction: out of the file and
            // into the tree on the directory holding it — or, when one is
            // already open somewhere, simply over to it. One tree.
            if cmd == TreeCmd::Up {
                match self.tree_window() {
                    Some(open) => self.set_focus(open),
                    None => self.show_tree_here(),
                }
            }
            return;
        }
        let Some(tree) = self.window_mut().tree_mut() else { return };

        // Everything the tree can answer without leaving itself.
        match cmd {
            TreeCmd::Select { down, count } => return tree.step(down, count),
            TreeCmd::First => return tree.select(0),
            TreeCmd::Last => return tree.select(usize::MAX),
            TreeCmd::HalfPage { down } => return tree.step(down, (height / 2).max(1)),
            TreeCmd::Collapse => return tree.collapse(),
            TreeCmd::Up => return tree.up(),
            TreeCmd::Down => return tree.down(),
            TreeCmd::Refresh => return tree.refresh(),
            TreeCmd::ToggleHidden => return tree.toggle_hidden(),
            TreeCmd::Prompt(op) => return self.prompt_file_op(op),
            TreeCmd::Delete => return self.delete_selected(),
            TreeCmd::Yank => return self.yank_selected_path(),
            TreeCmd::Mark(mode) => return self.mark_selected(mode),
            TreeCmd::ClearMarks => {
                if !self.session.clipboard.is_empty() {
                    self.session.clipboard.clear();
                    self.session.status = "marks cleared".into();
                }
                return;
            }
            TreeCmd::Paste => return self.paste_into_selected(),
            TreeCmd::Expand | TreeCmd::Enter => {}
        }

        let Some(row) = tree.selected_row() else { return };
        if row.kind == Kind::Dir {
            // The difference between the two keys is only here: `l` opens a
            // closed directory and leaves an open one alone, `Enter` flips it.
            match cmd {
                TreeCmd::Enter => tree.toggle(),
                _ => tree.expand(),
            }
            return;
        }

        let path = row.path.clone();
        let target = self.handoff_window();
        match self.open_path(&path.to_string_lossy()) {
            Ok(id) => {
                self.show(target, id);
                self.set_focus(target);
                self.session.status = self.name_of(id);
            }
            Err(e) => self.session.status = format!("{e:#}"),
        }
    }

    fn fresh_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_window);
        self.next_window += 1;
        id
    }

    fn run_window_cmd(&mut self, cmd: WindowCmd) {
        let focus = self.focus;
        let (area, chrome) = (self.area, self.chrome);

        match cmd {
            WindowCmd::Split { dir, path } => {
                // The content first: a failure to open must not leave a split
                // showing the wrong thing. `None` means a bare split, which
                // duplicates whatever this window holds rather than naming
                // something to go and find.
                let content = match path.as_deref() {
                    None => None,
                    Some(path) if std::path::Path::new(path).is_dir() => match Tree::new(path) {
                        Ok(tree) => Some(Content::Tree(tree)),
                        Err(e) => {
                            self.session.status = format!("{e:#}");
                            return;
                        }
                    },
                    Some(path) => match self.open_path(path) {
                        Ok(id) => Some(Content::Text(Text::new(id))),
                        Err(e) => {
                            self.session.status = format!("{e:#}");
                            return;
                        }
                    },
                };

                let new = self.fresh_window_id();
                if !self.layout.split(focus, new, dir, area, &chrome) {
                    // Hand the id back rather than leaving a hole in the
                    // sequence; nothing depends on it, but a gap invites the
                    // question of what used to be there.
                    self.next_window -= 1;
                    self.session.status = "not enough room to split".into();
                    return;
                }

                // Duplicates the window, so the split lands where you were
                // reading rather than at the top of the file.
                let mut window = self.window().clone();
                window.id = new;
                self.windows.push(window);
                self.set_focus(new);
                match content {
                    // Through `show`, so the new window records where the
                    // duplicated one was before it moves off that buffer.
                    Some(Content::Text(text)) => self.show(new, text.buffer),
                    Some(tree) => self.window_mut().show(tree),
                    None => {}
                }
            }

            WindowCmd::Tree => {
                // One tree, so the key that opened it is the key that puts it
                // away. Two trees are two of the same thing, and the second is
                // never the one you wanted.
                if let Some(open) = self.tree_window() {
                    return self.close_tree(open);
                }

                // Rooted before the split, because it is a fact about the
                // window you are leaving rather than the one being made.
                let path = self.buffer().and_then(|b| b.path.clone());
                let root = match path.as_ref().and_then(|p| p.parent()) {
                    Some(parent) => parent.to_path_buf(),
                    None => std::env::current_dir().unwrap_or_default(),
                };
                let tree = match Tree::new(&root) {
                    Ok(tree) => tree,
                    Err(e) => {
                        self.session.status = format!("{e:#}");
                        return;
                    }
                };

                let new = self.fresh_window_id();
                if !self.layout.split(focus, new, Dir::Vertical, area, &chrome) {
                    self.next_window -= 1;
                    self.session.status = "not enough room to split".into();
                    return;
                }
                self.windows.push(Window::showing(new, Content::Tree(tree)));
                self.set_focus(new);
                self.select_in_tree(path.as_deref());

                // A half-screen tree is not a sidebar. Narrowed to the width
                // the frontend asked for, which becomes a share of the
                // terminal from here on, like every other pane.
                let width = self
                    .layout
                    .rect_of(new, area, &chrome)
                    .map_or(0, |rect| rect.width)
                    .saturating_sub(chrome.tree_width);
                if width > 0 {
                    self.layout.resize(new, Dir::Vertical, -(width as i32), area, &chrome);
                }
            }

            WindowCmd::Focus(side) => {
                let anchor = self.anchor_of(focus, side);
                if let Some(next) = self.layout.neighbour(focus, side, area, &chrome, anchor) {
                    self.set_focus(next);
                }
            }
            WindowCmd::Cycle { back } => {
                if let Some(next) = self.layout.cycle(focus, back) {
                    self.set_focus(next);
                }
            }

            WindowCmd::Close => {
                self.close_window(focus);
            }
            WindowCmd::Only => {
                self.layout.only(focus);
                self.windows.retain(|w| w.id == focus);
            }

            WindowCmd::Resize { axis, cells } => {
                if !self.layout.resize(focus, axis, cells, area, &chrome) {
                    self.session.status = "no divider that way".into();
                }
            }
            WindowCmd::Equalize => self.layout.equalize(),
        }
    }

    /// Closes a window. Returns whether it could — the last one cannot.
    ///
    /// Never checks for unsaved changes: it discards nothing, because the
    /// buffer stays in the list. That is what "hidden buffer" means.
    fn close_window(&mut self, id: WindowId) -> bool {
        let Some(heir) = self.layout.close(id) else {
            self.session.status = "cannot close the last window".into();
            return false;
        };
        // Its cursor is worth keeping: reopening a split on the same file
        // should land where the closed one was looking.
        if let Some(text) = self.window_of(id).and_then(Window::text) {
            let (buffer, pairs) = (text.buffer, text.selections.as_pairs());
            self.entry_mut(buffer).last = pairs;
        }
        self.windows.retain(|w| w.id != id);
        if self.focus == id {
            self.set_focus(heir);
        }
        true
    }

    /// Where the cursor sits on the axis a directional switch travels across,
    /// in screen coordinates — what breaks the tie between two panes that are
    /// both to the right.
    fn anchor_of(&self, id: WindowId, side: Side) -> u16 {
        let Some(window) = self.window_of(id) else { return 0 };
        let Some(rect) = self.layout.rect_of(id, self.area, &self.chrome) else { return 0 };
        let Some(text) = window.text() else { return 0 };
        let buffer = &self.entry(text.buffer).buffer;
        let cursor = text.selections.cursor();
        match side {
            Side::Left | Side::Right => {
                let row = buffer.row_at(cursor).saturating_sub(text.scroll);
                rect.y + (row as u16).min(rect.height.saturating_sub(1))
            }
            Side::Up | Side::Down => {
                let col = buffer.col_at(cursor);
                rect.x + (col as u16).min(rect.width.saturating_sub(1))
            }
        }
    }

    /// The actions that touch only the session, and so need no view.
    ///
    /// The command line and the picker are session state, not the buffer's:
    /// they have to work in a window holding a tree, where there is no view to
    /// run anything in. Without this `:q` cannot leave `bee .`.
    ///
    /// Returns whether it handled the action.
    fn run_session_action(&mut self, action: &Action) -> bool {
        match action {
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
                if let Mode::Command(line) = &mut self.session.mode
                    && line.pop().is_none()
                {
                    self.session.mode = Mode::Normal;
                }
            }
            Action::CommandCancel => {
                self.session.mode = Mode::Normal;
                // The one `:` line that means something when abandoned.
                self.abandon_paste();
            }
            Action::CommandExecute => {
                let Mode::Command(line) = std::mem::take(&mut self.session.mode) else {
                    return true;
                };
                self.run_ex(&line);
            }

            Action::PickChar(c) => {
                if let Some(picker) = &mut self.session.picker {
                    picker.push_char(*c);
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
                if let Some(picker) = &mut self.session.picker {
                    picker.next();
                }
            }
            Action::PickPrev => {
                if let Some(picker) = &mut self.session.picker {
                    picker.prev();
                }
            }
            Action::PickToggleShort => {
                if let Some(picker) = &mut self.session.picker {
                    picker.toggle_short();
                }
            }
            Action::PickCancel => self.close_picker(),
            Action::PickAccept => self.accept_pick(),

            _ => return false,
        }
        true
    }

    fn close_picker(&mut self) {
        self.session.picker = None;
        self.session.mode = Mode::Normal;
    }

    /// Runs whatever the highlighted row meant.
    ///
    /// The two kinds part company here: a buffer pick reaches the list, which
    /// is the editor's, and a register pick pastes, which needs a view — and a
    /// tree pane has nothing to paste into.
    fn accept_pick(&mut self) {
        let picker = self.session.picker.take();
        self.session.mode = Mode::Normal;
        let Some(chosen) = picker.as_ref().and_then(Picker::selected) else { return };
        match picker.expect("checked above").kind {
            // A position rather than an id, because the rows were built from
            // the list in order and it cannot change while the picker holds
            // every key.
            PickerKind::Buffer => self.run_buffer_cmd(BufferCmd::Chosen(chosen)),
            PickerKind::Register { before } => {
                self.in_view(|view| view.paste_pick(chosen, before));
            }
        }
    }

    /// Runs `f` in the focused window's view, or says why it could not.
    ///
    /// The one place "this pane holds no text" is turned into a message, so no
    /// caller has to remember that a window might be a tree.
    fn in_view<T>(&mut self, f: impl FnOnce(&mut View) -> T) -> Option<T> {
        match self.focused() {
            Some(mut view) => Some(f(&mut view)),
            None => {
                self.session.status = "no buffer in this window".into();
                None
            }
        }
    }

    /// Closes the window when more than one is open, and quits only from the
    /// last — which is what vim does, and what makes `:qa` mean something
    /// different from `:q`.
    fn quit(&mut self, force: bool) {
        if self.windows.len() > 1 {
            self.close_window(self.focus);
        } else if self.buffer().is_some_and(Buffer::is_modified) && !force {
            self.session.status = "unsaved changes (use `:q!` to discard)".into();
        } else {
            self.session.quit = true;
        }
    }

    /// Every buffer has to agree, not just the focused one.
    fn quit_all(&mut self, force: bool) {
        let unsaved = self.buffers.iter().find(|b| b.buffer.is_modified()).map(|b| b.id);
        match unsaved {
            Some(id) if !force => {
                self.session.status =
                    format!("\"{}\" has unsaved changes (use `:qa!`)", self.name_of(id));
            }
            _ => self.session.quit = true,
        }
    }

    fn write_all(&mut self) {
        let ids: Vec<BufferId> = self
            .buffers
            .iter()
            .filter(|b| b.buffer.is_modified() && b.buffer.path.is_some())
            .map(|b| b.id)
            .collect();
        if ids.is_empty() {
            self.session.status = "nothing to write".into();
            return;
        }
        let mut written = 0;
        for id in &ids {
            let entry = self.entry_mut(*id);
            // No selections to record for a buffer nobody is looking at, and
            // the ones for a buffer in view have not moved.
            let pairs = entry.last.clone();
            match entry.buffer.save(pairs.clone(), pairs) {
                Ok(()) => written += 1,
                Err(e) => {
                    self.session.status = format!("error: {e:#}");
                    return;
                }
            }
        }
        self.session.status = format!("{written} written");
    }

    /// `:e <path>` — shows another file here.
    ///
    /// No longer refuses on unsaved changes: the old buffer goes hidden with
    /// its history and its modified flag intact, so nothing is discarded. Bare
    /// `:e` — reload from disk — still refuses, and needs a view.
    fn edit_path(&mut self, path: &str) {
        // A path is a path: `:e` asks the disk what it is rather than making
        // you remember which command a directory wants.
        if std::path::Path::new(path).is_dir() {
            return self.show_tree(path);
        }
        match self.open_path(path) {
            Ok(id) => {
                let focus = self.focus;
                self.show(focus, id);
                self.session.status = format!("\"{}\" loaded", self.name_of(id));
            }
            Err(e) => self.session.status = format!("{e:#}"),
        }
    }

    /// Points the focused window at a tree on `root`, parking what it held.
    fn show_tree(&mut self, root: &str) {
        let tree = match Tree::new(root) {
            Ok(tree) => tree,
            Err(e) => {
                self.session.status = format!("{e:#}");
                return;
            }
        };
        // Leaving a buffer still records where this window was in it, whether
        // what replaces it is another buffer or a tree.
        if let Some(text) = self.window().text() {
            let (buffer, pairs) = (text.buffer, text.selections.as_pairs());
            self.entry_mut(buffer).last = pairs;
        }
        self.session.status = tree.root().display().to_string();
        self.window_mut().show(Content::Tree(tree));
    }

    /// `-` from a text window: the tree on this file's directory, with the
    /// file selected. A `[No Name]` buffer has no directory, so it roots at
    /// the working directory instead.
    fn show_tree_here(&mut self) {
        let path = self.buffer().and_then(|b| b.path.clone());
        let root = match path.as_ref().and_then(|p| p.parent()) {
            Some(parent) => parent.to_path_buf(),
            None => std::env::current_dir().unwrap_or_default(),
        };

        self.show_tree(&root.to_string_lossy());
        self.select_in_tree(path.as_deref());
    }

    /// Puts the focused tree's selection on `path`, if it is showing it.
    ///
    /// What makes `-` and `Ctrl-W e` land where you were rather than at the
    /// top of the directory.
    fn select_in_tree(&mut self, path: Option<&std::path::Path>) {
        let (Some(path), Some(tree)) = (path, self.window_mut().tree_mut()) else { return };
        if let Some(row) = tree.rows().iter().position(|r| r.path == path) {
            tree.select(row);
        }
    }

    // ---- the filesystem -----------------------------------------------------

    /// Re-reads every open tree.
    ///
    /// All of them rather than only the ones showing the affected directory: a
    /// tree that cannot see the change re-reads to exactly the rows it had, so
    /// the bookkeeping to tell them apart would buy nothing.
    fn refresh_trees(&mut self) {
        for window in &mut self.windows {
            if let Some(tree) = window.tree_mut() {
                tree.refresh();
            }
        }
    }

    /// Reports what a filesystem call did, and re-reads the trees if it worked.
    fn report(&mut self, result: std::io::Result<()>, done: String) {
        self.session.status = match result {
            Ok(()) => {
                self.refresh_trees();
                done
            }
            Err(e) => format!("{e}"),
        };
    }

    /// `:create <path>` — an empty file, or a directory for a trailing slash.
    ///
    /// Intermediate directories are made along the way. Refusing because the
    /// parent is missing would be a message telling you to type two more
    /// commands you already asked for.
    fn create_path(&mut self, path: &str) {
        let target = std::path::Path::new(path);
        if target.exists() {
            self.session.status = format!("\"{path}\" already exists");
            return;
        }

        let result = if path.ends_with('/') || path.ends_with(std::path::MAIN_SEPARATOR) {
            std::fs::create_dir_all(target)
        } else {
            match target.parent().filter(|p| !p.as_os_str().is_empty()) {
                Some(parent) => std::fs::create_dir_all(parent),
                None => Ok(()),
            }
            .and_then(|()| std::fs::write(target, ""))
        };
        self.report(result, format!("\"{path}\" created"));
    }

    /// `:rename <old> <new>` — which is also how you move a file, since it is
    /// the same call and pretending otherwise would need two commands.
    fn rename_path(&mut self, from: &str, to: &str) {
        let (source, target) = (std::path::Path::new(from), std::path::Path::new(to));
        if !source.exists() {
            self.session.status = format!("\"{from}\" does not exist");
            return;
        }
        if target.exists() {
            self.session.status = format!("\"{to}\" already exists");
            return;
        }
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty())
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.session.status = format!("{e}");
            return;
        }

        if let Err(e) = std::fs::rename(source, target) {
            self.session.status = format!("{e}");
            return;
        }

        // A buffer left pointing at a path that no longer exists would recreate
        // the file under its old name on the next `:w`.
        for entry in &mut self.buffers {
            if entry.buffer.path.as_deref() == Some(source) {
                entry.buffer.path = Some(target.to_path_buf());
                // The extension may have changed, and with it the grammar.
                entry.syntax = syntax_for(&entry.buffer);
            }
        }
        self.report(Ok(()), format!("\"{from}\" → \"{to}\""));
    }

    /// `:delete[!] <path>` — a file, or a directory that `!` says may have
    /// something in it.
    ///
    /// An open buffer on the deleted file stays open: its text and history are
    /// intact and it simply no longer has a file, exactly as if it had never
    /// been saved. Closing a pane the user is reading is not what deleting a
    /// file asked for.
    fn delete_path(&mut self, path: &str, force: bool) {
        let target = std::path::Path::new(path);
        let Ok(meta) = std::fs::symlink_metadata(target) else {
            self.session.status = format!("\"{path}\" does not exist");
            return;
        };

        if self.modified_at(target) && !force {
            self.session.status = format!("\"{path}\" has unsaved changes (use `:delete!`)");
            return;
        }

        // A symlink is removed as the link it is, never followed into the
        // directory it points at — the same rule the tree draws it by.
        let result = if meta.is_dir() {
            let empty = std::fs::read_dir(target).map(|mut d| d.next().is_none()).unwrap_or(false);
            if !empty && !force {
                self.session.status = format!("\"{path}\" is not empty (use `:delete!`)");
                return;
            }
            std::fs::remove_dir_all(target)
        } else {
            std::fs::remove_file(target)
        };
        self.report(result, format!("\"{path}\" deleted"));
    }

    fn modified_at(&self, path: &std::path::Path) -> bool {
        self.buffers
            .iter()
            .any(|b| b.buffer.path.as_deref() == Some(path) && b.buffer.is_modified())
    }

    // ---- the clipboard ------------------------------------------------------

    /// `y` — the selected path into the register ring.
    ///
    /// The existing ring rather than one of its own: the point is that `p` in
    /// a text buffer then pastes the path. The tree produces for that ring and
    /// never reads from it.
    fn yank_selected_path(&mut self) {
        let Some(row) = self.window().tree().and_then(Tree::selected_row) else { return };
        let path = row.path.display().to_string();
        self.session.registers.push(Entry { text: path.clone(), kind: EntryKind::Charwise });
        self.session.status = format!("yanked {path}");
    }

    /// `c` / `x` — mark the selected path, or take the mark off.
    fn mark_selected(&mut self, mode: ClipMode) {
        let Some(row) = self.window().tree().and_then(Tree::selected_row) else { return };
        let path = row.path.clone();
        self.session.clipboard.mark(path, mode);
        // Which mode the set is in should never be something you have to
        // remember, so it is said every time it could have changed.
        self.session.status = self.session.clipboard.summary();
    }

    /// `p` — put what is marked into the directory the cursor is standing in.
    fn paste_into_selected(&mut self) {
        let Some(row) = self.window().tree().and_then(Tree::selected_row) else { return };
        let into = match row.kind {
            Kind::Dir => row.path.clone(),
            _ => row.path.parent().unwrap_or(&row.path).to_path_buf(),
        };
        self.paste_into(&into);
    }

    fn paste_into(&mut self, into: &std::path::Path) {
        if self.session.clipboard.is_empty() {
            self.session.status = "nothing marked".into();
            return;
        }
        let queue: Vec<std::path::PathBuf> = self.session.clipboard.paths().to_vec();

        // A directory cannot hold itself. That is a loop rather than a
        // mistake, and no name for the destination would fix it.
        if let Some(source) = queue.iter().find(|source| into.starts_with(source)) {
            self.session.status = format!("cannot paste \"{}\" into itself", source.display());
            return;
        }

        let mode = self.session.clipboard.mode();
        self.session.pasting = Some(Pasting { queue, into: into.to_path_buf(), mode, done: 0 });
        self.run_paste(None);
    }

    /// Places what is queued until something is in the way.
    ///
    /// `rename` is the destination for the head of the queue when the command
    /// line has just supplied one; otherwise each lands under its own name.
    fn run_paste(&mut self, rename: Option<std::path::PathBuf>) {
        let Some(mut pasting) = self.session.pasting.take() else { return };
        let mut rename = rename;

        while let Some(source) = pasting.queue.first().cloned() {
            let target = match rename.take() {
                Some(named) => named,
                None => match source.file_name() {
                    Some(name) => pasting.into.join(name),
                    None => {
                        pasting.queue.remove(0);
                        continue;
                    }
                },
            };

            if target.exists() {
                // Stop rather than overwrite, and ask. Esc on that line
                // abandons the rest — see `Action::CommandCancel`.
                self.session.mode = Mode::Command(format!("paste-as {}", target.display()));
                self.session.pasting = Some(pasting);
                return;
            }

            let result = match pasting.mode {
                ClipMode::Copy => copy_into(&source, &target),
                ClipMode::Cut => move_into(&source, &target),
            };
            if let Err(e) = result {
                self.session.status = format!("{e}");
                self.session.clipboard.clear();
                return;
            }
            pasting.queue.remove(0);
            pasting.done += 1;
        }

        let verb = match pasting.mode {
            ClipMode::Copy => "copied",
            ClipMode::Cut => "moved",
        };
        // A cut is spent — the sources are not there any more. A copy is not,
        // so the same set can go to a second place.
        if pasting.mode == ClipMode::Cut {
            self.session.clipboard.clear();
        }
        self.session.status = format!("{} {verb} into {}", pasting.done, pasting.into.display());
        self.refresh_trees();
    }

    /// `Esc` on the conflict line. Abandons what is left rather than skipping
    /// one, which is why there is no skip: ten clashes would be ten decisions.
    fn abandon_paste(&mut self) {
        let Some(pasting) = self.session.pasting.take() else { return };
        self.session.status = format!("paste abandoned after {}", pasting.done);
        self.refresh_trees();
    }

    /// `dd` — deletes the selected path with no `:` line in between.
    ///
    /// Irreversible: there is no undo for the filesystem and nothing is moved
    /// aside first. What survives is `delete_path`'s two guards — a directory
    /// with anything in it, and an open buffer with unsaved changes, both still
    /// want `:delete!` — and the root row, which is the directory you are
    /// standing in and never what `dd` meant.
    fn delete_selected(&mut self) {
        let Some(row) = self.window().tree().and_then(Tree::selected_row) else { return };
        if row.depth == 0 {
            self.session.status = "that is the root of this tree".into();
            return;
        }
        let path = row.path.display().to_string();
        self.delete_path(&path, false);
    }

    /// `a` `r` — puts the selected path on the command line and leaves it
    /// there.
    ///
    /// A prefilled line *is* the confirmation: the editor has no prompt
    /// machinery and gains none here. You see the path, and Enter is the
    /// assent; where that is not enough, the guard is `!`.
    fn prompt_file_op(&mut self, op: FileOp) {
        let Some(row) = self.window().tree().and_then(Tree::selected_row) else { return };
        let path = row.path.display().to_string();
        let line = match op {
            // Inside a directory, beside a file — which is where you meant.
            FileOp::Create => {
                let dir = match row.kind {
                    Kind::Dir => row.path.clone(),
                    _ => row.path.parent().unwrap_or(&row.path).to_path_buf(),
                };
                format!("create {}/", dir.display())
            }
            FileOp::Rename => format!("rename {path} {path}"),
        };
        self.session.status.clear();
        self.session.mode = Mode::Command(line);
    }

    /// Runs a `:` line.
    ///
    /// Parsed before anything is dispatched, rather than discovered inside a
    /// view: a tree window has no view for a `:` line to land in, and the
    /// window and buffer commands no longer have to travel back out as
    /// escalations to reach the lists they change. See `docs/specs/tree.md`.
    pub fn run_ex(&mut self, line: &str) {
        let Some(parsed) = parse_ex(line) else { return };
        match parsed {
            ExLine::Window(cmd) => self.run_window_cmd(cmd),
            ExLine::Buffer(cmd) => self.run_buffer_cmd(cmd),
            ExLine::Edit { path } => self.edit_path(&path),
            ExLine::Quit { force } => self.quit(force),
            ExLine::QuitAll { force } => self.quit_all(force),
            ExLine::WriteAll => self.write_all(),
            ExLine::Highlight(on) => self.session.highlight_search = on,
            ExLine::Set(arg) => self.set_option(&arg),
            ExLine::Create(path) => self.create_path(&path),
            ExLine::Rename { from, to } => self.rename_path(&from, &to),
            ExLine::Delete { path, force } => self.delete_path(&path, force),
            ExLine::Paste(dir) => match dir {
                Some(dir) => self.paste_into(std::path::Path::new(&dir)),
                None => self.paste_into_selected(),
            },
            ExLine::PasteAs(path) => self.run_paste(Some(path.into())),
            ExLine::Unknown(name) => self.session.status = format!("not a command: {name}"),
            ExLine::Error(message) => self.session.status = message,

            // The rest need the rope, and so need a view.
            ExLine::Write(path) => {
                self.in_view(|view| view.write(&path));
            }
            ExLine::Reload { force } => {
                self.in_view(|view| view.edit(force));
            }
            ExLine::Goto(row) => {
                self.in_view(|view| view.goto_row(row));
            }
            ExLine::WriteQuit(path) => {
                if self.in_view(|view| view.write(&path)) == Some(true) {
                    self.quit(true);
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

    // ---- what the frontend and embedders call -------------------------------

    /// Lays the window tree out in `area` and returns one rect per window, in
    /// draw order.
    ///
    /// The frontend passes the area it owns and the chrome it intends to draw;
    /// how much of a pane is text is then its own decision, which it reports
    /// back through [`Editor::size_window`]. Keeping that split is what stops a
    /// status row — a terminal convention — from being baked into geometry.
    pub fn layout(&mut self, area: Rect, chrome: Chrome) -> Vec<(WindowId, Rect)> {
        // Remembered, because `Ctrl-W +` means one row and a row is only a
        // fraction of a weight once you know how many rows the parent had.
        self.area = area;
        self.chrome = chrome;
        self.layout.rects(area, &chrome)
    }

    /// Tells a window how much room it actually got, and scrolls it to its
    /// cursor. Called once per window per frame.
    pub fn size_window(&mut self, id: WindowId, width: usize, height: usize) {
        if let Some(window) = self.window_mut_of(id) {
            window.width = width;
            window.height = height;
            // A tree scrolls to its selected row, the same job by another name
            // — and the height is what `Ctrl-D` halves in either.
            if let Some(tree) = window.tree_mut() {
                tree.scroll_to_selected(height);
                return;
            }
        }
        if let Some(mut view) = self.view(id) {
            view.scroll_to_cursor(height);
        }
    }

    /// Runs a command against the focused window.
    ///
    /// Two entry points, both explicit. Commands that change the buffer list or
    /// the window tree are matched here, *before* a view exists, because a view
    /// borrows from what they change. Everything else goes through the view,
    /// which hands back anything it discovered mid-flight — an ex line is only
    /// read once it is already running inside one.
    pub fn apply(&mut self, cmd: Command) {
        match cmd.action {
            Action::Buffer(buffer_cmd) => return self.run_buffer_cmd(buffer_cmd),
            Action::Window(window_cmd) => return self.run_window_cmd(window_cmd),
            Action::Tree(tree_cmd) => return self.run_tree_cmd(tree_cmd),
            _ => {}
        }
        if self.run_session_action(&cmd.action) {
            return;
        }
        // What is left needs the rope, and a tree window has none.
        if let Some(mut view) = self.focused() {
            view.apply(cmd);
        }
    }

    /// Settles everything an edit leaves behind. Called once per key, after the
    /// command has been applied and before the frame is drawn.
    ///
    /// One drain of `pending_edits`, two consumers: the parse tree, and every
    /// *other* window showing that buffer, whose cursors and scroll rows have
    /// to move with text they did not edit. LSP `didChange` is the third, and
    /// hangs off this same drain — see README decision #2.
    ///
    /// Shifting rather than clamping is the point. Clamping would keep the
    /// other window inside the rope, but its cursor would slide relative to the
    /// text every time a line was inserted above it, which is precisely what a
    /// second window on one file exists to avoid.
    pub fn settle(&mut self) {
        let focus = self.focus;
        for entry in &mut self.buffers {
            let edits = std::mem::take(&mut entry.buffer.pending_edits);
            if edits.is_empty() {
                continue;
            }
            if let Some(syntax) = &mut entry.syntax {
                syntax.update(entry.buffer.rope(), &edits);
            }

            // Text windows only: a tree pane shows no rope and has nothing in
            // it that an edit could move.
            for window in self.windows.iter_mut() {
                // The window that made the edit already has the right cursor:
                // the command that moved the text moved it too. Mapping it
                // again would double-count.
                if window.id == focus {
                    continue;
                }
                let Some(text) = window.text_mut() else { continue };
                if text.buffer != entry.id {
                    continue;
                }

                let mapped: Vec<Selection> = text
                    .selections
                    .all()
                    .iter()
                    .map(|s| Selection {
                        anchor: Cursor::at(edits.iter().fold(s.anchor.at, |at, e| e.map(at))),
                        head: Cursor::at(edits.iter().fold(s.head.at, |at, e| e.map(at))),
                    })
                    .collect();
                text.selections.set(mapped);

                // The scroll row follows the text above it, so a line inserted
                // over the top of an unfocused pane does not slide its view.
                let start = entry
                    .buffer
                    .rope()
                    .line_to_char(text.scroll.min(entry.buffer.line_count().saturating_sub(1)));
                let moved = edits.iter().fold(start, |at, e| e.map(at));
                text.scroll = entry.buffer.row_at(Cursor::at(moved));
            }
        }
    }

    /// `None` in a tree pane, which has a selected row but no cursor.
    pub fn cursor(&self) -> Option<Cursor> {
        Some(self.selections()?.cursor())
    }

    pub fn cursor_row(&self) -> Option<usize> {
        Some(self.buffer()?.row_at(self.cursor()?))
    }

    pub fn cursor_col(&self) -> Option<usize> {
        Some(self.buffer()?.col_at(self.cursor()?))
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        if let Some(mut view) = self.focused() {
            view.set_cursor(cursor);
        }
    }

    /// No block where there is no selection, which is not the same as a lie
    /// about where one is — hence a empty list rather than a zero span.
    pub fn block_spans(&self) -> Vec<(usize, usize)> {
        let (Some(buffer), Some(selections)) = (self.buffer(), self.selections()) else {
            return Vec::new();
        };
        spans_of_block(buffer, selections, self.session.block_to_eol)
    }

    pub fn block_span_at(&self, row: usize) -> (usize, usize) {
        let (Some(buffer), Some(selections)) = (self.buffer(), self.selections()) else {
            return (0, 0);
        };
        span_of_block_at(buffer, selections, self.session.block_to_eol, row)
    }

    pub fn search_count(&mut self) -> Option<(usize, usize)> {
        self.focused().and_then(|mut view| view.search_count())
    }

    pub fn scroll_to_cursor(&mut self, height: usize) {
        if let Some(mut view) = self.focused() {
            view.scroll_to_cursor(height);
        }
    }
}

impl View<'_> {
    /// Rebuilds the parse tree from scratch. The old one belongs to text that
    /// no longer exists, and a new path can change the language outright.
    fn reload_syntax(&mut self) {
        *self.syntax = syntax_for(self.buffer);
    }

    /// Everything that needs the rope. What does not — the window tree, the
    /// buffer list, the command line, the picker — was handled by `Editor`
    /// before this view existed.
    pub fn apply(&mut self, cmd: Command) {
        if let Action::RepeatChange { count } = cmd.action {
            self.session.search_focus = false;
            self.repeat_change(count);
            return;
        }
        if self.session.undo_from.is_empty() {
            self.session.undo_from = self.selections.as_pairs();
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
            let after = self.selections.as_pairs();
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
        let selection = self.selections.primary();
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
        block_columns_of(self.buffer, self.selections)
    }

    pub fn block_spans(&self) -> Vec<(usize, usize)> {
        spans_of_block(self.buffer, self.selections, self.session.block_to_eol)
    }

    pub fn block_span_at(&self, row: usize) -> (usize, usize) {
        span_of_block_at(self.buffer, self.selections, self.session.block_to_eol, row)
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
        self.selections.all().iter().flat_map(|selection| self.rows_of(*selection)).collect()
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
            self.selections.set(cursors);
        } else {
            self.session.mode = Mode::Normal;
            *self.selections = Selections::single(self.buffer.clamped(Cursor::at(top_left), false));
        }
    }

    /// Where the block's cursors go for `I` and `A`.
    ///
    /// `I` skips a row that does not reach the left edge; `A` pads one out to
    /// the column so what is appended lines up. Vim pads on `Esc`, bee pads on
    /// entry — the same edit, visible while it is being typed into.
    fn block_insert_columns(&mut self, append: bool) -> Vec<Cursor> {
        let (lo, hi) = self.selections.primary().range();
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

        // Nothing `.` can replay escalates: a change is text, and the commands
        // that reach the session are not recorded as one.
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
        let mut list: Vec<Selection> = self.selections.all().to_vec();
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

        self.selections.set(list);
    }

    /// The selections to record as a revision's `before` and `after`.
    ///
    /// `undo_from` is captured at the start of each command and cleared when a
    /// group closes, so a group that spans several commands — a typing run —
    /// still reports where it started.
    fn undo_bounds(&mut self) -> (Cursors, Cursors) {
        let after = self.selections.as_pairs();
        (std::mem::take(&mut self.session.undo_from), after)
    }

    /// The primary cursor — what a single-cursor operation acts on.
    pub fn cursor(&self) -> Cursor {
        self.selections.cursor()
    }

    pub fn cursor_row(&self) -> usize {
        self.buffer.row_at(self.cursor())
    }

    pub fn cursor_col(&self) -> usize {
        self.buffer.col_at(self.cursor())
    }

    /// Collapses to a single cursor at `cursor`.
    pub fn set_cursor(&mut self, cursor: Cursor) {
        *self.selections = Selections::single(cursor);
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
                let at = self.selections.cursor().at;
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
                self.selections.collapse_each();
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
                    self.selections.collapse_each();
                    Mode::Normal
                } else {
                    Mode::Visual(*kind)
                };
                if *kind == VisualKind::Block {
                    // The rectangle is derived from one selection's corners,
                    // so a block is single-selection by construction.
                    self.selections.collapse_to_primary();
                    self.session.block_to_eol = false;
                }
            }
            Action::SwapEnds => {
                self.for_each_selection(|_, sel| sel.flipped());
            }
            Action::SwapCorners => {
                let selection = self.selections.primary();
                let (anchor_row, head_row) =
                    (self.buffer.row_at(selection.anchor), self.buffer.row_at(selection.head));
                let (anchor_col, head_col) =
                    (self.buffer.col_at(selection.anchor), self.buffer.col_at(selection.head));
                let anchor = self.at_row_col(anchor_row, head_col);
                let head = self.at_row_col(head_row, anchor_col);
                *self.selections.primary_mut() = Selection { anchor, head };
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
                self.selections.set(cursors.into_iter().map(Selection::collapsed).collect());
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
                let primary = self.selections.primary();
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
                self.selections.push(match self.session.mode.visual() {
                    Some(_) => {
                        Selection { anchor: Cursor::at(found), head: Cursor::at(found + width - 1) }
                    }
                    None => Selection::at(found),
                });
            }
            Action::AddCursorLine { below } => {
                let primary = self.selections.primary();
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
                self.selections.push(Selection::at(line_start + col));
            }
            Action::CollapseCursors => self.selections.collapse_to_primary(),
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
                let at = self.selections.cursor();
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
                let half = (*self.height / 2).max(1) as isize;
                self.scroll_by(if *down { half } else { -half }, true);
            }

            // Handled by `Editor` before this view was built: the window
            // tree, the buffer list, the command line and the picker.
            Action::EnterCommandMode
            | Action::CommandChar(_)
            | Action::CommandBackspace
            | Action::CommandCancel
            | Action::CommandExecute
            | Action::PickChar(_)
            | Action::PickBackspace
            | Action::PickNext
            | Action::PickPrev
            | Action::PickToggleShort
            | Action::PickCancel
            | Action::PickAccept
            | Action::Buffer(_)
            | Action::Window(_)
            | Action::Tree(_) => {}
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
        *self.selections = Selections::from_pairs(pairs);
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
        let at = self.selections.cursor().at;
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
        let height = *self.height;
        if height == 0 {
            return;
        }
        let last = self.buffer.line_count().saturating_sub(1);
        let max_scroll = self.buffer.line_count().saturating_sub(height);
        (*self.scroll) = (*self.scroll).saturating_add_signed(lines).min(max_scroll);

        let row = self.buffer.row_at(self.selections.cursor());
        // The cursor has to end up inside the window *including* the scrolloff
        // margin. Leave it in the margin and `scroll_to_cursor` — which runs
        // every frame — immediately drags the window back, undoing the scroll.
        let margin = Self::margin(height);
        let top = ((*self.scroll) + margin).min(last);
        let bottom = ((*self.scroll) + height).saturating_sub(margin + 1).min(last);

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

    /// Pastes the register the picker landed on.
    fn paste_pick(&mut self, chosen: usize, before: bool) {
        let Some(entry) = self.session.registers.get(chosen).cloned() else { return };
        // Push before pasting: move-to-front makes this the ring's head, so
        // `.` and a later bare `p` repeat the entry you chose rather than
        // whatever happened to be most recent.
        self.session.registers.push(entry.clone());
        let landed = self.buffer.paste(self.selections.cursor(), &entry, before, 1);
        *self.selections = Selections::single(landed);
    }

    /// The `:` commands. Deliberately tiny — this is not where the editor gets
    /// interesting, and a real command table wants the config layer first.
    /// `:42` — put the cursor on that row.
    fn goto_row(&mut self, row: usize) {
        let cursor = self.buffer.at_row(row.saturating_sub(1), false);
        *self.selections = Selections::single(cursor);
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

    /// `:e` reloads this buffer from disk; `:e!` reloads discarding changes.
    ///
    /// Still refuses when the buffer is modified, unlike `:e <path>`: this one
    /// genuinely throws work away, where opening another file merely hides
    /// this one.
    ///
    /// The parse tree has to be rebuilt rather than patched — it belongs to
    /// text that no longer exists.
    fn edit(&mut self, force: bool) {
        if self.buffer.is_modified() && !force {
            self.session.status = "unsaved changes (use `:e!` to discard)".into();
            return;
        }

        let at = self.selections.cursor();
        match self.buffer.reload(at) {
            Ok(cursor) => {
                *self.selections = Selections::single(cursor);
                self.reload_syntax();
                let name =
                    self.buffer.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.session.status = format!("\"{name}\" loaded");
            }
            Err(e) => self.session.status = format!("{e:#}"),
        }
    }

    /// Keeps the cursor inside a `height`-row viewport, with a scrolloff margin.
    pub fn scroll_to_cursor(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        *self.height = height;
        let row = self.buffer.row_at(self.selections.cursor());
        let margin = Self::margin(height);

        if row < (*self.scroll) + margin {
            (*self.scroll) = row.saturating_sub(margin);
        } else if row + margin >= (*self.scroll) + height {
            (*self.scroll) = row + margin + 1 - height;
        }

        let max_scroll = self.buffer.line_count().saturating_sub(height);
        (*self.scroll) = (*self.scroll).min(max_scroll);
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
            let from = ed.cursor().unwrap();
            let at = ed.buffer_mut().unwrap().insert_str(from, text);
            ed.set_cursor(at);
            let pairs = ed.selections().unwrap().as_pairs();
            ed.buffer_mut().unwrap().commit_undo(pairs.clone(), pairs);
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "f");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdef", "5x is one unit, not five");
    }

    #[test]
    fn a_whole_insert_session_undoes_as_one_unit() {
        let mut ed = editor("");
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "hello");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "hello");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "", "all five chars, not one");
    }

    /// `o` edits *and* enters insert mode. The newline it inserts belongs to the
    /// same undo unit as everything typed after it.
    #[test]
    fn open_line_and_what_follows_it_undo_together() {
        let mut ed = editor("a");
        ed.apply(cmd(Action::OpenLineBelow));
        type_str(&mut ed, "bc");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\nbc");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a", "the newline went back too");
    }

    #[test]
    fn entering_and_leaving_insert_without_typing_is_not_an_undo_step() {
        let mut ed = editor("a");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(cmd(Action::EnterInsert));
        ed.apply(cmd(Action::EnterNormal));

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a", "one undo reaches the delete");
    }

    #[test]
    fn undo_takes_a_count() {
        let mut ed = editor("abcdef");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "def");

        ed.apply(Command { count: 3, action: Action::Undo });
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdef");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "baz");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "foo bar baz", "both words, one undo");
    }

    #[test]
    fn c_enters_insert_mode_so_you_can_type_the_replacement() {
        let mut ed = editor("foo bar");
        ed.apply(operate(Operator::Change, Motion::WordForward, 1));
        assert_eq!(ed.session.mode, Mode::Insert);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), " bar");

        type_str(&mut ed, "xyz");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "xyz bar");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "foo bar");
    }

    #[test]
    fn a_delete_that_matches_nothing_leaves_no_undo_step() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::WordBackward, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "bc", "b at char 0 did nothing");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abc", "one undo still reaches the x");
    }

    fn paste(before: bool, count: usize) -> Command {
        cmd(Action::Paste { before, count })
    }

    #[test]
    fn yank_then_paste_round_trips() {
        let mut ed = editor("foo bar");
        ed.apply(operate(Operator::Yank, Motion::WordForward, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "foo bar", "yank changed nothing");

        ed.apply(paste(false, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "ffoo oo bar");
    }

    #[test]
    fn a_delete_fills_the_ring_so_p_puts_it_back() {
        let mut ed = editor("one\ntwo");
        ed.apply(operate(Operator::Delete, Motion::CurrentLine, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "two");

        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\ntwo", "linewise, so above");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "keep", "the junk line is gone");

        ed.apply(paste(false, 1));
        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "keep\nkeep",
            "the ring still holds the yank, not the junk"
        );
    }

    #[test]
    fn pasting_from_an_empty_ring_says_so() {
        let mut ed = editor("abc");
        ed.apply(paste(false, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abc");
        assert_eq!(ed.session.status, "nothing to paste");
    }

    #[test]
    fn a_paste_is_one_undo_step_even_with_a_count() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Yank, Motion::Right, 1));
        ed.apply(paste(false, 3));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "aaaabc");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abc");
    }

    /// Undo puts the text back in the buffer and leaves the ring alone, as vim
    /// does — you can undo a delete and still paste what it took.
    #[test]
    fn undo_does_not_roll_back_the_ring() {
        let mut ed = editor("one\ntwo");
        ed.apply(operate(Operator::Delete, Motion::CurrentLine, 1));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\ntwo");

        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\none\ntwo");
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
            let c = ed.buffer().unwrap().at_row(row, false);
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
        let c = ed.buffer().unwrap().at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickNext, Action::PickAccept]);

        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.session.picker.is_none());
        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "alpha\nalpha\nbeta\ngamma",
            "the third-newest entry, chosen by moving down twice"
        );
    }

    #[test]
    fn typing_in_the_picker_filters_what_accept_takes() {
        let mut ed = ed_with_ring();
        let c = ed.buffer().unwrap().at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickChar('b'), Action::PickChar('e'), Action::PickAccept]);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "beta\nalpha\nbeta\ngamma");
    }

    /// Choosing promotes the entry, so a plain `p` afterwards repeats it — this
    /// is what makes `.` work without re-opening the picker.
    #[test]
    fn accepting_moves_the_entry_to_the_front_of_the_ring() {
        let mut ed = ed_with_ring();
        let c = ed.buffer().unwrap().at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickNext, Action::PickAccept]);

        assert_eq!(ed.session.registers.front().unwrap().text, "alpha\n");
        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "alpha\nalpha\nalpha\nbeta\ngamma");
    }

    #[test]
    fn cancelling_leaves_the_buffer_and_the_ring_alone() {
        let mut ed = ed_with_ring();
        let before = ed.buffer().unwrap().rope().to_string();
        ed.apply(open_register_picker(false));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickCancel]);

        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.session.picker.is_none());
        assert_eq!(ed.buffer().unwrap().rope().to_string(), before);
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
        let before = ed.buffer().unwrap().rope().to_string();
        let c = ed.buffer().unwrap().at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickAccept]);
        assert_ne!(ed.buffer().unwrap().rope().to_string(), before);

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), before);
    }

    #[test]
    fn redo_walks_back_forward() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(cmd(Action::Undo));
        ed.apply(cmd(Action::Redo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "bc");
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

    /// A directory under the temp dir, gone when the test ends.
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("bee-dir-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(self, rel: &str) -> Self {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
            self
        }

        fn dir(self, rel: &str) -> Self {
            std::fs::create_dir_all(self.0.join(rel)).unwrap();
            self
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn editing_a_directory_shows_a_tree_rather_than_reading_it_as_text() {
        let d = ScratchDir::new("edit").file("a.rs");
        let mut ed = editor("hello");

        ex(&mut ed, &format!("e {}", d.path()));

        assert!(ed.window().tree().is_some(), "the window holds a tree");
        assert!(ed.buffer().is_none(), "so it shows no buffer at all");
    }

    /// `bee .` — and the buffer list stays non-empty, so nothing downstream
    /// has to learn that the session began on a directory.
    #[test]
    fn opening_a_directory_starts_on_a_tree() {
        let d = ScratchDir::new("open").file("a.rs");

        let ed = Editor::open(d.path()).unwrap();

        assert!(ed.window().tree().is_some());
        assert_eq!(ed.buffer_ids().len(), 1, "with a [No Name] behind it");
    }

    #[test]
    fn splitting_on_a_directory_gives_the_new_window_a_tree() {
        let d = ScratchDir::new("split").file("a.rs");
        let mut ed = editor("hello");
        sized(&mut ed);

        ex(&mut ed, &format!("vs {}", d.path()));

        assert_eq!(ed.window_ids().len(), 2);
        assert!(ed.window().tree().is_some(), "focus follows the new window");
    }

    /// `-` is one key in two directions: down into a file from the tree, and
    /// back out to the tree from the file.
    #[test]
    fn minus_in_a_text_window_opens_the_tree_on_this_files_directory() {
        let d = ScratchDir::new("minus").file("a.rs").file("b.rs");
        let mut ed = Editor::open(format!("{}/b.rs", d.path())).unwrap();

        ed.apply(cmd(Action::Tree(TreeCmd::Up)));

        let tree = ed.window().tree().expect("a tree now");
        assert_eq!(tree.root(), std::path::Path::new(d.path()));
        assert_eq!(tree.selected_row().unwrap().name, "b.rs", "on the file you left");
    }

    /// A given window's view onto its buffer — what the tests that watch a
    /// second pane are actually asking about.
    fn text_of(ed: &Editor, id: WindowId) -> &Text {
        ed.window_of(id).expect("no such window").text().expect("that window holds a tree")
    }

    /// The command line is session state, not the buffer's, so it has to work
    /// where there is no buffer. Without this `:q` cannot leave `bee .`.
    #[test]
    fn the_command_line_works_in_a_tree_window() {
        let d = ScratchDir::new("tree-ex").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        ed.apply(cmd(Action::EnterCommandMode));
        for c in "q".chars() {
            ed.apply(cmd(Action::CommandChar(c)));
        }
        ed.apply(cmd(Action::CommandExecute));

        assert!(ed.session.quit, "`:q` left the editor");
    }

    /// The picker is session state too, and `:ls` is reachable from a tree.
    #[test]
    fn the_buffer_picker_works_from_a_tree_window() {
        let d = ScratchDir::new("tree-pick").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        ex(&mut ed, "ls");
        assert!(ed.session.picker.is_some(), "`:ls` opened the picker");
        ed.apply(cmd(Action::PickAccept));

        assert!(ed.window().buffer().is_some(), "accepting showed the buffer here");
    }

    /// Displacing the tree is only tolerable because it comes straight back —
    /// re-reading the directory and losing what you had open would make the
    /// single-window flow a one-way trip.
    #[test]
    fn the_alternate_brings_a_displaced_tree_back_with_its_expansion() {
        let d = ScratchDir::new("alt-tree").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Expand);
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Enter);
        assert!(ed.window().buffer().is_some(), "the file displaced the tree");

        ed.apply(cmd(Action::Buffer(BufferCmd::Alternate)));

        let tree = ed.window().tree().expect("the tree came back");
        assert_eq!(tree.rows().len(), 3, "root, pkg/ and a.rs — still expanded");
    }

    /// A tree pane shows no buffer, so the lines that need one say so rather
    /// than failing quietly.
    #[test]
    fn the_lines_that_need_a_buffer_say_when_there_is_none() {
        let d = ScratchDir::new("tree-refuse").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        ex(&mut ed, "w");
        assert_eq!(ed.session.status, "no buffer in this window");

        ex(&mut ed, "bd");
        assert_eq!(ed.session.status, "no buffer in this window");
    }

    /// `:bn` is not refused, though — asking to see a buffer here is exactly
    /// what it means, and the tree becomes the alternate.
    #[test]
    fn switching_buffers_in_a_tree_window_shows_one_and_parks_the_tree() {
        let d = ScratchDir::new("tree-bn").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        ex(&mut ed, "bn");

        assert!(ed.window().buffer().is_some(), "a buffer is showing");
        assert!(matches!(ed.window().alt, Some(Content::Tree(_))), "the tree is the alternate");
    }

    // ---- file operations ----------------------------------------------------

    #[test]
    fn create_makes_the_intermediate_directories_and_refuses_to_overwrite() {
        let d = ScratchDir::new("create");
        let mut ed = Editor::open(d.path()).unwrap();

        ex(&mut ed, &format!("create {}/a/b/c.rs", d.path()));
        assert!(std::path::Path::new(&format!("{}/a/b/c.rs", d.path())).is_file());

        ex(&mut ed, &format!("create {}/a/b/c.rs", d.path()));
        assert!(ed.session.status.contains("exists"), "{}", ed.session.status);
    }

    /// A trailing slash is what tells a directory from a file — the one bit of
    /// syntax the three commands have.
    #[test]
    fn a_trailing_slash_creates_a_directory() {
        let d = ScratchDir::new("create-dir");
        let mut ed = Editor::open(d.path()).unwrap();

        ex(&mut ed, &format!("create {}/pkg/", d.path()));

        assert!(std::path::Path::new(&format!("{}/pkg", d.path())).is_dir());
    }

    /// Leaving the buffer pointing at a path that no longer exists means the
    /// next `:w` recreates the file under its old name.
    #[test]
    fn rename_moves_an_open_buffer_with_the_file_and_repicks_its_syntax() {
        let d = ScratchDir::new("rename").file("a.txt");
        let (from, to) = (format!("{}/a.txt", d.path()), format!("{}/b.rs", d.path()));
        let mut ed = Editor::open(&from).unwrap();
        assert!(ed.syntax().is_none(), "no grammar for .txt");

        ex(&mut ed, &format!("rename {from} {to}"));

        assert!(std::path::Path::new(&to).is_file(), "moved on disk");
        assert!(!std::path::Path::new(&from).exists());
        assert_eq!(ed.name_of(ed.window().buffer().unwrap()), to, "and the buffer came with it");
        assert!(ed.syntax().is_some(), "a .rs now, so it highlights");
    }

    #[test]
    fn delete_needs_a_bang_for_a_directory_with_anything_in_it() {
        let d = ScratchDir::new("delete").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        let pkg = format!("{}/pkg", d.path());

        ex(&mut ed, &format!("delete {pkg}"));
        assert!(std::path::Path::new(&pkg).is_dir(), "refused: {}", ed.session.status);
        assert!(ed.session.status.contains("not empty"), "{}", ed.session.status);

        ex(&mut ed, &format!("delete! {pkg}"));
        assert!(!std::path::Path::new(&pkg).exists());
    }

    /// Deleting the file out from under a pane the user is reading is not a
    /// reason to close it — the text and its history are still there.
    #[test]
    fn deleting_a_file_leaves_its_buffer_open() {
        let d = ScratchDir::new("delete-open").file("a.rs");
        let path = format!("{}/a.rs", d.path());
        let mut ed = Editor::open(&path).unwrap();

        ex(&mut ed, &format!("delete {path}"));

        assert!(!std::path::Path::new(&path).exists());
        assert_eq!(ed.buffer_ids().len(), 1, "the buffer is still open");
    }

    #[test]
    fn a_file_operation_refreshes_every_tree_that_can_see_it() {
        let d = ScratchDir::new("refresh-trees").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        assert_eq!(ed.window().tree().unwrap().rows().len(), 2, "root plus a.rs");

        ex(&mut ed, &format!("create {}/b.rs", d.path()));

        assert_eq!(ed.window().tree().unwrap().rows().len(), 3, "without pressing R");
    }

    #[test]
    fn the_file_op_keys_prefill_the_command_line_with_the_selected_path() {
        let d = ScratchDir::new("prompt").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Prompt(FileOp::Rename));

        let Mode::Command(line) = &ed.session.mode else { panic!("not on the command line") };
        let path = format!("{}/a.rs", d.path());
        assert_eq!(line, &format!("rename {path} {path}"), "edit the second one");
    }

    // ---- the clipboard ------------------------------------------------------

    fn marked(ed: &Editor) -> Vec<String> {
        ed.session
            .clipboard
            .paths()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// The tree produces for the register ring and never consumes from it: the
    /// point is that `p` in a *text* buffer then pastes the path.
    #[test]
    fn y_yanks_the_selected_path_into_the_register_ring() {
        let d = ScratchDir::new("yank").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Yank);

        let want = format!("{}/a.rs", d.path());
        assert_eq!(ed.session.registers.front().unwrap().text, want);
        assert!(ed.session.status.contains(&want), "says what it took: {}", ed.session.status);
    }

    #[test]
    fn c_and_x_mark_and_the_footer_says_which() {
        let d = ScratchDir::new("mark").file("a.rs").file("b.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Copy));
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Copy));

        assert_eq!(marked(&ed), ["a.rs", "b.rs"]);
        assert_eq!(ed.session.status, "2 to copy");

        tree_key(&mut ed, TreeCmd::ClearMarks);
        assert!(ed.session.clipboard.is_empty(), "Esc clears them");
    }

    #[test]
    fn p_copies_the_marked_files_into_the_selected_directory() {
        let d = ScratchDir::new("paste-copy").file("a.rs").dir("pkg");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed); // pkg/ sorts first
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Copy));
        tree_key(&mut ed, TreeCmd::Select { down: false, count: 1 });

        tree_key(&mut ed, TreeCmd::Paste);

        assert!(std::path::Path::new(&format!("{}/pkg/a.rs", d.path())).is_file());
        assert!(std::path::Path::new(&format!("{}/a.rs", d.path())).is_file(), "still there");
        assert!(!ed.session.clipboard.is_empty(), "a copy keeps the set for a second place");
    }

    #[test]
    fn p_moves_a_cut_and_forgets_it_afterwards() {
        let d = ScratchDir::new("paste-cut").file("a.rs").dir("pkg");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Cut));
        tree_key(&mut ed, TreeCmd::Select { down: false, count: 1 });

        tree_key(&mut ed, TreeCmd::Paste);

        assert!(std::path::Path::new(&format!("{}/pkg/a.rs", d.path())).is_file());
        assert!(!std::path::Path::new(&format!("{}/a.rs", d.path())).exists(), "moved");
        assert!(ed.session.clipboard.is_empty(), "the sources are not there any more");
    }

    #[test]
    fn a_conflict_stops_the_paste_and_offers_the_name_on_the_command_line() {
        let d = ScratchDir::new("paste-clash").file("a.rs").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed); // pkg/
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Copy));
        tree_key(&mut ed, TreeCmd::Select { down: false, count: 1 });

        tree_key(&mut ed, TreeCmd::Paste);

        let Mode::Command(line) = &ed.session.mode else {
            panic!("no prompt: {:?}", ed.session.mode)
        };
        assert_eq!(line, &format!("paste-as {}/pkg/a.rs", d.path()));
    }

    #[test]
    fn paste_as_places_the_one_that_stopped_and_carries_on() {
        let d = ScratchDir::new("paste-as").file("a.rs").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Copy));
        tree_key(&mut ed, TreeCmd::Select { down: false, count: 1 });
        tree_key(&mut ed, TreeCmd::Paste);

        ex(&mut ed, &format!("paste-as {}/pkg/renamed.rs", d.path()));

        assert!(std::path::Path::new(&format!("{}/pkg/renamed.rs", d.path())).is_file());
        assert!(ed.session.pasting.is_none(), "and the paste is finished");
    }

    /// Esc aborts the run rather than skipping one file, which is why there is
    /// no skip: ten clashes would otherwise be ten decisions.
    #[test]
    fn escape_on_the_conflict_line_abandons_the_rest_of_the_paste() {
        let d = ScratchDir::new("paste-abort").file("a.rs").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Copy));
        tree_key(&mut ed, TreeCmd::Select { down: false, count: 1 });
        tree_key(&mut ed, TreeCmd::Paste);

        ed.apply(cmd(Action::CommandCancel));

        assert!(ed.session.pasting.is_none());
        assert!(ed.session.status.contains("abandoned"), "{}", ed.session.status);
    }

    /// A loop rather than a mistake: no name for the destination fixes it.
    #[test]
    fn a_directory_cannot_be_pasted_inside_itself() {
        let d = ScratchDir::new("paste-loop").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed); // pkg/
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Copy));
        tree_key(&mut ed, TreeCmd::Paste);

        assert!(ed.session.status.contains("into itself"), "{}", ed.session.status);
        assert!(!std::path::Path::new(&format!("{}/pkg/pkg", d.path())).exists());
    }

    /// `dd` is the one thing in the tree with no `:` line in front of it.
    #[test]
    fn dd_deletes_the_selected_file_outright() {
        let d = ScratchDir::new("dd").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Delete);

        assert!(!std::path::Path::new(&format!("{}/a.rs", d.path())).exists());
        assert_eq!(ed.window().tree().unwrap().rows().len(), 1, "and the row is gone");
    }

    /// The root row is the directory you are standing in, and never what `dd`
    /// meant — the two guards in `:delete` cannot catch this one.
    #[test]
    fn dd_refuses_the_root_of_the_tree() {
        let d = ScratchDir::new("dd-root").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        tree_key(&mut ed, TreeCmd::Delete);

        assert!(std::path::Path::new(d.path()).is_dir(), "{}", ed.session.status);
        assert_eq!(ed.session.status, "that is the root of this tree");
    }

    /// `dd` keeps `:delete`'s guards: a directory with anything in it still
    /// wants the bang, typed out in full.
    #[test]
    fn dd_still_refuses_a_directory_with_anything_in_it() {
        let d = ScratchDir::new("dd-dir").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Delete);

        assert!(std::path::Path::new(&format!("{}/pkg", d.path())).is_dir());
        assert!(ed.session.status.contains("not empty"), "{}", ed.session.status);
    }

    #[test]
    fn plus_re_roots_into_the_selected_directory() {
        let d = ScratchDir::new("plus").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Down);

        let tree = ed.window().tree().unwrap();
        assert_eq!(tree.root(), std::path::Path::new(&format!("{}/pkg", d.path())));
    }

    #[test]
    fn create_from_the_tree_offers_the_directory_you_are_standing_in() {
        let d = ScratchDir::new("prompt-create").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Prompt(FileOp::Create));

        let Mode::Command(line) = &ed.session.mode else { panic!("not on the command line") };
        assert_eq!(line, &format!("create {}/pkg/", d.path()), "inside it, not beside it");
    }

    /// `Ctrl-W e` — a tree beside the file you are reading, rooted at its
    /// directory with the file selected, and narrow enough to be a sidebar.
    #[test]
    fn ctrl_w_e_opens_a_narrow_tree_beside_the_current_file() {
        let d = ScratchDir::new("sidebar").file("a.rs").file("b.rs");
        let mut ed = Editor::open(format!("{}/b.rs", d.path())).unwrap();
        let file = ed.focus();
        sized(&mut ed);

        ed.apply(cmd(Action::Window(WindowCmd::Tree)));
        let tree_window = ed.focus();

        assert_eq!(ed.window_ids().len(), 2);
        let tree = ed.window().tree().expect("focus is on the new tree");
        assert_eq!(tree.root(), std::path::Path::new(d.path()).canonicalize().unwrap());
        assert_eq!(tree.selected_row().unwrap().name, "b.rs", "on the file you were in");

        let at = |ed: &mut Editor, cols| {
            let rects = ed.layout(Rect::new(0, 0, cols, 24), TEST_CHROME);
            let of = |id| rects.iter().find(|(w, _)| *w == id).unwrap().1.width;
            (of(tree_window), of(file))
        };

        // `sized` laid out at 80, which is the width the sidebar was cut to.
        let (sidebar, text) = at(&mut ed, 80);
        assert!(sidebar < text, "the tree is the narrower pane");
        assert!(sidebar <= TEST_CHROME.tree_width + 1, "and a sidebar, not a half: {sidebar}");

        // A share of the terminal from here on, like every other pane.
        let (wide, _) = at(&mut ed, 160);
        assert!(wide > sidebar, "it grew with the terminal rather than staying put");
    }

    /// One tree, and the same key that opened it puts it away. Two trees are
    /// two of the same thing, and the second one is never the one you wanted.
    #[test]
    fn ctrl_w_e_closes_the_tree_it_opened() {
        let d = ScratchDir::new("toggle").file("a.rs");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        let file = ed.focus();
        sized(&mut ed);

        ed.apply(cmd(Action::Window(WindowCmd::Tree)));
        assert_eq!(ed.window_ids().len(), 2, "opened");

        ed.apply(cmd(Action::Window(WindowCmd::Tree)));

        assert_eq!(ed.window_ids().len(), 1, "and put away again");
        assert_eq!(ed.focus(), file);
        assert!(ed.tree_window().is_none());
    }

    /// Including from the tree itself, which is where pressing it twice in a
    /// row lands you.
    #[test]
    fn ctrl_w_e_closes_the_tree_from_inside_it() {
        let d = ScratchDir::new("toggle-inside").file("a.rs");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        sized(&mut ed);
        ed.apply(cmd(Action::Window(WindowCmd::Tree)));
        assert!(ed.window().tree().is_some(), "focus is on the tree");

        ed.apply(cmd(Action::Window(WindowCmd::Tree)));

        assert_eq!(ed.window_ids().len(), 1);
        assert!(ed.tree_window().is_none());
    }

    /// The `bee .` session: the tree is the only window, so there is nothing
    /// to close. It shows a buffer instead, which is the same thing Enter on a
    /// file does and leaves the session in a state that still has a window.
    #[test]
    fn toggling_off_the_only_window_shows_a_buffer_rather_than_closing_it() {
        let d = ScratchDir::new("toggle-alone").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        sized(&mut ed);

        ed.apply(cmd(Action::Window(WindowCmd::Tree)));

        assert_eq!(ed.window_ids().len(), 1, "the last window is never closed");
        assert!(ed.window().buffer().is_some(), "showing a buffer now");
    }

    /// `-` with a tree already open goes to it rather than making a second.
    #[test]
    fn minus_goes_to_the_tree_that_is_already_open() {
        let d = ScratchDir::new("minus-existing").file("a.rs");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        sized(&mut ed);
        ed.apply(cmd(Action::Window(WindowCmd::Tree)));
        let tree = ed.focus();
        ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        assert_ne!(ed.focus(), tree);

        ed.apply(cmd(Action::Tree(TreeCmd::Up)));

        assert_eq!(ed.focus(), tree, "focused the one that exists");
        assert_eq!(ed.window_ids().len(), 2, "rather than making another");
    }

    /// The file it opens still goes back where you came from — the sidebar is
    /// a window like any other, and needs no rule of its own.
    #[test]
    fn a_sidebar_tree_hands_files_back_to_the_pane_it_grew_from() {
        let d = ScratchDir::new("sidebar-open").file("a.rs").file("b.rs");
        let mut ed = Editor::open(format!("{}/b.rs", d.path())).unwrap();
        let file = ed.focus();
        sized(&mut ed);
        ed.apply(cmd(Action::Window(WindowCmd::Tree)));
        let tree = ed.focus();

        tree_key(&mut ed, TreeCmd::First);
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Enter);

        assert!(ed.window_of(tree).unwrap().tree().is_some(), "the sidebar stayed");
        assert_eq!(ed.focus(), file, "and the file landed where you came from");
        assert!(ed.name_of(ed.window().buffer().unwrap()).ends_with("a.rs"));
    }

    /// Three panes: two files and a tree. Enter has to pick one of the two,
    /// and the one you came from is the one you meant.
    #[test]
    fn enter_returns_to_whichever_window_you_reached_the_tree_from() {
        let d = ScratchDir::new("handoff").file("a.rs");
        let mut ed = editor("one");
        sized(&mut ed);
        let first = ed.focus();
        ed.apply(cmd(Action::Window(WindowCmd::Split { dir: Dir::Vertical, path: None })));
        let second = ed.focus();
        assert_ne!(first, second);

        ex(&mut ed, &format!("vs {}", d.path()));
        let tree = ed.focus();
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Enter);
        assert_eq!(ed.focus(), second, "came from the second, went back to it");

        // Now reach the tree from the *first* window instead.
        ed.apply(cmd(Action::Window(WindowCmd::Focus(Side::Left))));
        while ed.focus() != first {
            ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        }
        while ed.focus() != tree {
            ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        }
        tree_key(&mut ed, TreeCmd::Enter);

        assert_eq!(ed.focus(), first, "and this time back to the first");
    }

    fn tree_key(ed: &mut Editor, tree_cmd: TreeCmd) {
        ed.apply(cmd(Action::Tree(tree_cmd)));
    }

    /// Down one row from the root, onto the first entry.
    fn select_first_entry(ed: &mut Editor) {
        tree_key(ed, TreeCmd::Select { down: true, count: 1 });
    }

    #[test]
    fn enter_on_a_file_replaces_the_tree_when_there_is_nowhere_else_to_put_it() {
        let d = ScratchDir::new("enter-alone").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Enter);

        let shown = ed.name_of(ed.window().buffer().expect("a buffer now"));
        assert!(shown.ends_with("a.rs"), "opened here: {shown}");
        assert!(
            matches!(ed.window().alt, Some(Content::Tree(_))),
            "and the tree is the alternate, expansion and all",
        );
    }

    #[test]
    fn enter_on_a_file_hands_it_to_the_other_window_and_leaves_the_tree_alone() {
        let d = ScratchDir::new("enter-split").file("a.rs");
        let mut ed = editor("hello");
        sized(&mut ed);
        let text_window = ed.focus();
        ex(&mut ed, &format!("vs {}", d.path()));
        let tree_window = ed.focus();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Enter);

        assert!(
            ed.window_of(tree_window).unwrap().tree().is_some(),
            "the tree pane survives, which is what makes `:vs .` a sidebar",
        );
        assert_eq!(ed.focus(), text_window, "and focus follows the file");
        let shown = ed.name_of(ed.window().buffer().unwrap());
        assert!(shown.ends_with("a.rs"), "which landed in the other pane: {shown}");
    }

    /// A line that parses but cannot run says what it wanted, not that it was
    /// never a command. Easy to lose when the parse moved out of the view.
    #[test]
    fn the_alternate_answers_to_b_hash_with_no_space() {
        let (a, b) = (Scratch::new("hash-a", "a\n"), Scratch::new("hash-b", "b\n"));
        let mut ed = Editor::open(a.path()).unwrap();
        ex(&mut ed, &format!("e {}", b.path()));

        ex(&mut ed, "b#");

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\n", "{}", ed.session.status);
    }

    #[test]
    fn a_command_missing_its_argument_says_what_it_wanted() {
        let mut ed = editor("hello");

        ex(&mut ed, "b");

        assert_eq!(ed.session.status, "which buffer?");
    }

    #[test]
    fn e_rereads_the_file_from_disk() {
        let f = Scratch::new("reload.txt", "before\n");
        let mut ed = opened(&f);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "before\n");

        f.write("after\n");
        ex(&mut ed, "e");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "after\n");
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
            ed.buffer().unwrap().rope().to_string().contains("local edit"),
            "the buffer must be left alone when the reload is refused",
        );
    }

    #[test]
    fn e_bang_discards_them() {
        let f = Scratch::new("force.txt", "on disk\n");
        let mut ed = opened(&f);
        type_str(&mut ed, "local edit");

        ex(&mut ed, "e!");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "on disk\n");
        assert!(!ed.buffer().unwrap().is_modified(), "a fresh read is not a modified buffer");
    }

    #[test]
    fn a_reload_drops_undo_history_rather_than_replaying_gone_text() {
        let f = Scratch::new("history.txt", "one\n");
        let mut ed = opened(&f);
        type_str(&mut ed, "typed");
        let pairs = ed.selections().unwrap().as_pairs();
        ed.buffer_mut().unwrap().commit_undo(pairs.clone(), pairs);

        f.write("two\n");
        ex(&mut ed, "e!");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "two\n");

        // Undoing here must not resurrect text from the previous file.
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "two\n");
    }

    #[test]
    fn e_with_a_path_edits_that_file_instead() {
        let a = Scratch::new("a.txt", "file a\n");
        let b = Scratch::new("b.txt", "file b\n");
        let mut ed = opened(&a);

        ex(&mut ed, &format!("e {}", b.path()));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "file b\n");
        assert_eq!(ed.buffer().unwrap().path.as_deref(), Some(std::path::Path::new(b.path())));
    }

    #[test]
    fn a_shorter_file_does_not_leave_the_cursor_past_the_end() {
        let f = Scratch::new("shrink.txt", "one\ntwo\nthree\nfour\n");
        let mut ed = opened(&f);
        let c = ed.buffer().unwrap().at_row(3, false);
        ed.set_cursor(c);

        f.write("x\n");
        ex(&mut ed, "e!");
        assert!(
            ed.cursor().unwrap().at <= ed.buffer().unwrap().rope().len_chars(),
            "cursor {} is past the end of a {}-char buffer",
            ed.cursor().unwrap().at,
            ed.buffer().unwrap().rope().len_chars(),
        );
        assert_eq!(ed.cursor_row().unwrap(), 0);
    }

    #[test]
    fn a_reload_rebuilds_the_parse_tree_rather_than_patching_it() {
        let f = Scratch::new("tree.rs", "fn a() {}\n");
        let mut ed = opened(&f);
        assert!(ed.syntax().is_some(), "a .rs file should have a grammar");

        f.write("struct B;\n");
        ex(&mut ed, "e!");

        // A tree left over from the old text would disagree with the rope.
        let rope = ed.buffer().unwrap().rope();
        let spans = ed.syntax().as_ref().unwrap().highlights(rope, 0..rope.len_bytes());
        assert!(
            spans.iter().all(|s| s.end_byte <= rope.len_bytes()),
            "highlight spans point past the end of the reloaded text",
        );
    }

    #[test]
    fn e_on_a_buffer_with_no_file_name_reports_rather_than_panicking() {
        let mut ed = editor("scratch");
        let pairs = ed.selections().unwrap().as_pairs();
        ed.buffer_mut().unwrap().commit_undo(pairs.clone(), pairs);
        ex(&mut ed, "e");
        assert!(!ed.session.status.is_empty(), "should say something");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "scratch");
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
        assert!(!ed.buffer().unwrap().is_modified());

        ex(&mut ed, "qall");
        assert!(ed.session.quit);
    }

    /// The point of the `a` forms, now that they are not aliases: they answer
    /// for buffers no window is showing.
    #[test]
    fn qa_and_wa_reach_a_buffer_nobody_is_looking_at() {
        let a = Scratch::new("qa_a.txt", "a\n");
        let b = Scratch::new("qa_b.txt", "b\n");
        let mut ed = opened(&a);

        // Dirty the first buffer, then move the only window off it.
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));
        ex(&mut ed, &format!("e {}", b.path()));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "b\n", "looking at the second file");

        ex(&mut ed, "qa");
        assert!(!ed.session.quit, "the hidden buffer still has unsaved changes");
        assert!(
            ed.session.status.contains("qa_a.txt"),
            "and `:qa` names it: {}",
            ed.session.status
        );

        ex(&mut ed, "wa");
        assert_eq!(a.read(), "Xa\n", "`:wa` wrote the buffer no window is showing");

        ex(&mut ed, "qa");
        assert!(ed.session.quit);
    }

    // ---- windows -----------------------------------------------------------

    const TEST_CHROME: Chrome =
        Chrome { columns: 1, rows: 0, min_width: 8, min_height: 2, tree_width: 30 };

    /// Gives the editor a screen to lay out in. Splitting, resizing and
    /// directional switching are geometry, and geometry needs a size.
    fn sized(ed: &mut Editor) {
        ed.layout(Rect::new(0, 0, 80, 24), TEST_CHROME);
    }

    fn split(ed: &mut Editor, dir: Dir) {
        sized(ed);
        // The frontend settles after every key. Without doing the same, edits
        // from the test's own setup are still pending, and the next settle
        // would replay them onto the window that did not make them.
        ed.settle();
        ed.apply(cmd(Action::Window(WindowCmd::Split { dir, path: None })));
    }

    /// The window that is not focused.
    fn other(ed: &Editor) -> WindowId {
        *ed.window_ids().iter().find(|&&id| id != ed.focus()).expect("expected a second window")
    }

    #[test]
    fn a_split_gives_two_windows_onto_one_buffer() {
        let mut ed = editor("one\ntwo\n");
        split(&mut ed, Dir::Vertical);

        assert_eq!(ed.window_ids().len(), 2);
        assert_eq!(ed.buffer_ids().len(), 1, "one file, two views of it");
        assert_eq!(Some(text_of(&ed, other(&ed)).buffer), ed.window().buffer());
    }

    /// Vim copies the cursor into the new window, so the split lands on the
    /// line you were reading rather than at the top of the file.
    #[test]
    fn a_split_lands_where_you_were_reading() {
        let mut ed = editor("one\ntwo\nthree\nfour\n");
        let at = ed.buffer().unwrap().at_row(2, false);
        ed.set_cursor(at);

        split(&mut ed, Dir::Horizontal);
        assert_eq!(ed.cursor_row().unwrap(), 2, "the new window");
        assert_eq!(text_of(&ed, other(&ed)).selections.cursor(), at, "and the old one");
    }

    /// The reason `Edit` carries a char range at all: a window that did not
    /// make the edit still has to follow the text.
    #[test]
    fn editing_in_one_window_moves_the_cursor_in_the_other() {
        let mut ed = editor("one\ntwo\nthree\nfour\n");
        let at = ed.buffer().unwrap().at_row(2, false);
        ed.set_cursor(at);
        split(&mut ed, Dir::Horizontal);

        let watcher = other(&ed);
        let row_of = |ed: &Editor| {
            ed.buffer_of(watcher).unwrap().row_at(text_of(ed, watcher).selections.cursor())
        };
        assert_eq!(row_of(&ed), 2);

        // Insert a line above everything, from the focused window.
        ed.set_cursor(Cursor::at(0));
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::InsertNewline));
        ed.apply(cmd(Action::EnterNormal));
        ed.settle();

        let (buffer, text) = (ed.buffer_of(watcher).unwrap(), text_of(&ed, watcher));
        assert_eq!(
            buffer.row_at(text.selections.cursor()),
            3,
            "the other window followed the text down, rather than staying on row 2",
        );
    }

    /// Undo replays through `edit_raw`, so it produces edits like any other
    /// change — and the other window follows without undo knowing it exists.
    #[test]
    fn an_undo_in_one_window_moves_the_other_too() {
        let mut ed = editor("one\ntwo\nthree\nfour\n");
        let at = ed.buffer().unwrap().at_row(2, false);
        ed.set_cursor(at);
        split(&mut ed, Dir::Horizontal);
        let watcher = other(&ed);

        ed.set_cursor(Cursor::at(0));
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::InsertNewline));
        ed.apply(cmd(Action::EnterNormal));
        ed.settle();
        let moved = text_of(&ed, watcher).selections.cursor();

        ed.apply(cmd(Action::Undo));
        ed.settle();

        let (buffer, text) = (ed.buffer_of(watcher).unwrap(), text_of(&ed, watcher));
        assert_ne!(text.selections.cursor(), moved, "it moved back");
        assert_eq!(buffer.row_at(text.selections.cursor()), 2);
    }

    #[test]
    fn a_second_window_scrolls_on_its_own() {
        let mut ed = editor(&"line\n".repeat(200));
        split(&mut ed, Dir::Vertical);
        let watcher = other(&ed);

        let at = ed.buffer().unwrap().at_row(150, false);
        ed.set_cursor(at);
        sized(&mut ed);
        for id in ed.window_ids() {
            ed.size_window(id, 40, 20);
        }

        assert!(ed.window().text().unwrap().scroll > 100, "the focused window followed its cursor");
        assert_eq!(text_of(&ed, watcher).scroll, 0, "and the other one stayed put");
    }

    #[test]
    fn closing_a_window_leaves_its_buffer_open() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);

        ed.apply(cmd(Action::Window(WindowCmd::Close)));
        assert_eq!(ed.window_ids().len(), 1);
        assert_eq!(ed.buffer_ids().len(), 1);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "text\n");
    }

    /// Closing discards nothing — the buffer stays in the list — so it has no
    /// business asking about unsaved changes.
    #[test]
    fn closing_a_window_does_not_ask_about_unsaved_changes() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));

        ed.apply(cmd(Action::Window(WindowCmd::Close)));
        assert_eq!(ed.window_ids().len(), 1, "closed without complaint");
        assert!(ed.buffer().unwrap().is_modified(), "and the changes are still there");
    }

    #[test]
    fn the_last_window_cannot_be_closed() {
        let mut ed = editor("text\n");
        sized(&mut ed);

        ed.apply(cmd(Action::Window(WindowCmd::Close)));
        assert_eq!(ed.window_ids().len(), 1);
        assert!(ed.session.status.contains("last window"));
    }

    #[test]
    fn only_closes_every_other_window() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);
        split(&mut ed, Dir::Horizontal);
        assert_eq!(ed.window_ids().len(), 3);

        let kept = ed.focus();
        ed.apply(cmd(Action::Window(WindowCmd::Only)));
        assert_eq!(ed.window_ids(), vec![kept]);
    }

    /// This is what makes `:qa` mean something different from `:q`.
    #[test]
    fn q_closes_a_window_and_quits_only_from_the_last() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);

        ex(&mut ed, "q");
        assert!(!ed.session.quit, "it closed a window rather than quitting");
        assert_eq!(ed.window_ids().len(), 1);

        // From the last window it is a quit again, unsaved-changes check and
        // all — which the closes above deliberately skipped.
        ex(&mut ed, "q");
        assert!(!ed.session.quit);
        assert!(ed.session.status.contains("unsaved"));

        ex(&mut ed, "q!");
        assert!(ed.session.quit);
    }

    #[test]
    fn switching_moves_focus_without_touching_the_buffer() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);
        let first = ed.focus();

        // The new window is on the left, so its neighbour is to the right.
        ed.apply(cmd(Action::Window(WindowCmd::Focus(Side::Right))));
        assert_ne!(ed.focus(), first);

        ed.apply(cmd(Action::Window(WindowCmd::Focus(Side::Left))));
        assert_eq!(ed.focus(), first, "and back");
    }

    #[test]
    fn there_is_nothing_past_the_edge_so_focus_stays_put() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);
        let first = ed.focus();

        ed.apply(cmd(Action::Window(WindowCmd::Focus(Side::Left))));
        assert_eq!(ed.focus(), first);
    }

    #[test]
    fn cycling_reaches_every_window_and_wraps() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);
        split(&mut ed, Dir::Vertical);

        let mut seen = vec![ed.focus()];
        for _ in 0..2 {
            ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
            seen.push(ed.focus());
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 3, "cycling visited all three");

        ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        assert_eq!(ed.window_ids().len(), 3, "and wrapped rather than stopping");
    }

    #[test]
    fn a_split_with_no_room_says_so_rather_than_making_a_pane_of_nothing() {
        let mut ed = editor("text\n");
        // Three rows: one pane fits a status row and a line of text; two do not.
        ed.layout(Rect::new(0, 0, 80, 3), TEST_CHROME);

        ed.apply(cmd(Action::Window(WindowCmd::Split { dir: Dir::Horizontal, path: None })));
        assert_eq!(ed.window_ids().len(), 1);
        assert!(ed.session.status.contains("not enough room"));
    }

    #[test]
    fn splitting_with_a_path_opens_that_file_in_the_new_window() {
        let a = Scratch::new("split_a.txt", "a\n");
        let b = Scratch::new("split_b.txt", "b\n");
        let mut ed = opened(&a);
        sized(&mut ed);

        ex(&mut ed, &format!("vs {}", b.path()));
        assert_eq!(ed.window_ids().len(), 2);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "b\n", "the new window shows it");
        assert_eq!(
            ed.buffer_of(other(&ed)).unwrap().rope().to_string(),
            "a\n",
            "and the old one is untouched",
        );
    }

    #[test]
    fn resizing_moves_the_divider_and_equalize_puts_it_back() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);

        let width = |ed: &mut Editor| {
            let focus = ed.focus();
            ed.layout(Rect::new(0, 0, 80, 24), TEST_CHROME)
                .into_iter()
                .find(|&(id, _)| id == focus)
                .map(|(_, r)| r.width)
                .unwrap()
        };
        let before = width(&mut ed);

        ed.apply(cmd(Action::Window(WindowCmd::Resize { axis: Dir::Vertical, cells: 6 })));
        assert_eq!(width(&mut ed), before + 6);

        ed.apply(cmd(Action::Window(WindowCmd::Equalize)));
        assert_eq!(width(&mut ed), before);
    }

    // ---- the buffer list ---------------------------------------------------

    #[test]
    fn e_on_an_open_path_reuses_the_buffer_rather_than_loading_it_twice() {
        let a = Scratch::new("reuse_a.txt", "a\n");
        let b = Scratch::new("reuse_b.txt", "b\n");
        let mut ed = opened(&a);

        ex(&mut ed, &format!("e {}", b.path()));
        assert_eq!(ed.buffer_ids().len(), 2);

        ex(&mut ed, &format!("e {}", a.path()));
        assert_eq!(ed.buffer_ids().len(), 2, "back to the first, not a third copy of it");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\n");
    }

    /// Two live ropes over one path is the bug reuse exists to prevent: an edit
    /// made through one would be invisible to the other, and `:w` would pick a
    /// winner.
    #[test]
    fn a_reused_buffer_keeps_the_edits_made_in_it() {
        let a = Scratch::new("keep_a.txt", "a\n");
        let b = Scratch::new("keep_b.txt", "b\n");
        let mut ed = opened(&a);

        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));
        ex(&mut ed, &format!("e {}", b.path()));
        ex(&mut ed, &format!("e {}", a.path()));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xa\n");
        assert!(ed.buffer().unwrap().is_modified(), "and it is still dirty");
    }

    #[test]
    fn e_with_a_path_no_longer_refuses_over_unsaved_changes() {
        let a = Scratch::new("hide_a.txt", "a\n");
        let b = Scratch::new("hide_b.txt", "b\n");
        let mut ed = opened(&a);
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));

        // Nothing is discarded — the old buffer goes hidden, dirty and intact.
        ex(&mut ed, &format!("e {}", b.path()));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "b\n");

        // But a reload still throws work away, so it still refuses.
        ex(&mut ed, &format!("e {}", a.path()));
        ex(&mut ed, "e");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xa\n", "`:e` refused");
        assert!(ed.session.status.contains("unsaved"));
    }

    #[test]
    fn bn_and_bp_cycle_the_list_and_wrap() {
        let a = Scratch::new("cyc_a.txt", "a\n");
        let b = Scratch::new("cyc_b.txt", "b\n");
        let c = Scratch::new("cyc_c.txt", "c\n");
        let mut ed = opened(&a);
        ex(&mut ed, &format!("e {}", b.path()));
        ex(&mut ed, &format!("e {}", c.path()));

        let text = |ed: &Editor| ed.buffer().unwrap().rope().to_string();
        assert_eq!(text(&ed), "c\n");

        ex(&mut ed, "bn");
        assert_eq!(text(&ed), "a\n", "past the end and round to the front");
        ex(&mut ed, "bp");
        assert_eq!(text(&ed), "c\n", "and back the other way");
        ex(&mut ed, "bp");
        assert_eq!(text(&ed), "b\n");
    }

    #[test]
    fn cycling_away_and_back_keeps_your_place() {
        let a = Scratch::new("place_a.txt", "one\ntwo\nthree\nfour\n");
        let b = Scratch::new("place_b.txt", "b\n");
        let mut ed = opened(&a);

        let at = ed.buffer().unwrap().at_row(2, false);
        ed.set_cursor(at);
        assert_eq!(ed.cursor_row().unwrap(), 2);

        ex(&mut ed, &format!("e {}", b.path()));
        assert_eq!(ed.cursor_row().unwrap(), 0, "a different file, so a different place");

        ex(&mut ed, "bp");
        assert_eq!(ed.cursor_row().unwrap(), 2, "back where we were reading");
    }

    #[test]
    fn ctrl_caret_swaps_between_the_last_two() {
        let a = Scratch::new("alt_a.txt", "a\n");
        let b = Scratch::new("alt_b.txt", "b\n");
        let mut ed = opened(&a);
        ex(&mut ed, &format!("e {}", b.path()));

        ed.apply(cmd(Action::Buffer(BufferCmd::Alternate)));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\n");
        ed.apply(cmd(Action::Buffer(BufferCmd::Alternate)));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "b\n", "and back — it is a swap");
    }

    #[test]
    fn b_with_a_partial_path_matches_one_buffer_or_says_which() {
        let a = Scratch::new("uniq_alpha.txt", "a\n");
        let b = Scratch::new("uniq_beta.txt", "b\n");
        let mut ed = opened(&a);
        ex(&mut ed, &format!("e {}", b.path()));

        ex(&mut ed, "b alpha");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\n");

        // Both paths contain "uniq_", so this is ambiguous and must not guess.
        ex(&mut ed, "b uniq_");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\n", "stayed put");
        assert!(ed.session.status.contains("more than one"), "{}", ed.session.status);

        ex(&mut ed, "b nowhere");
        assert!(ed.session.status.contains("no buffer matching"));
    }

    #[test]
    fn bd_falls_through_to_the_next_buffer_without_closing_anything() {
        let a = Scratch::new("del_a.txt", "a\n");
        let b = Scratch::new("del_b.txt", "b\n");
        let mut ed = opened(&a);
        ex(&mut ed, &format!("e {}", b.path()));

        ex(&mut ed, "bd");
        assert_eq!(ed.buffer_ids().len(), 1);
        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "a\n",
            "the window fell to the next one"
        );
    }

    #[test]
    fn bd_refuses_over_unsaved_changes_until_forced() {
        let f = Scratch::new("del_dirty.txt", "text\n");
        let mut ed = opened(&f);
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));

        ex(&mut ed, "bd");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xtext\n", "still here");
        assert!(ed.session.status.contains("unsaved"));

        ex(&mut ed, "bd!");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "", "discarded for a fresh buffer");
    }

    /// The list is never empty, so `Editor::buffer()` is always valid and no
    /// path has to handle a session with nothing open.
    #[test]
    fn deleting_the_last_buffer_leaves_a_fresh_one() {
        let f = Scratch::new("last.txt", "text\n");
        let mut ed = opened(&f);

        ex(&mut ed, "bd");
        assert_eq!(ed.buffer_ids().len(), 1);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "");
        assert_eq!(ed.buffer().unwrap().path, None, "and it is a no-name buffer");
    }

    /// A stable id that resolves to nothing is the one way it is worse than an
    /// index, which at least fails loudly.
    #[test]
    fn deleting_a_buffer_clears_it_from_the_alternate_slot() {
        let a = Scratch::new("dangle_a.txt", "a\n");
        let b = Scratch::new("dangle_b.txt", "b\n");
        let c = Scratch::new("dangle_c.txt", "c\n");
        let mut ed = opened(&a);
        ex(&mut ed, &format!("e {}", b.path()));
        ex(&mut ed, &format!("e {}", c.path()));

        // Alternate is b; delete it out from under the slot.
        assert_eq!(ed.window().alt_buffer().map(|id| ed.name_of(id)), Some(b.path().to_string()));
        ex(&mut ed, "b dangle_b");
        ex(&mut ed, "bd");

        assert!(!ed.buffer_ids().iter().any(|&id| ed.name_of(id).contains("dangle_b")));
        assert_eq!(ed.window().alt_buffer(), None, "the slot that named it was cleared");

        let showing = ed.buffer().unwrap().rope().to_string();
        ed.apply(cmd(Action::Buffer(BufferCmd::Alternate)));
        assert_eq!(ed.session.status, "no alternate buffer");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), showing, "and nothing moved");
    }

    #[test]
    fn ls_opens_the_picker_over_the_list_and_accepting_switches() {
        let a = Scratch::new("pick_a.txt", "a\n");
        let b = Scratch::new("pick_b.txt", "b\n");
        let mut ed = opened(&a);
        ex(&mut ed, &format!("e {}", b.path()));

        ex(&mut ed, "ls");
        assert_eq!(ed.session.mode, Mode::Pick);
        assert!(ed.session.picker.is_some());

        // First row is the first buffer in the list.
        ed.apply(cmd(Action::PickAccept));
        assert_eq!(ed.session.mode, Mode::Normal);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\n");
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
        ed.window_mut()
            .text_mut()
            .unwrap()
            .selections
            .set(positions.iter().map(|&p| Selection::at(p)).collect());
        ed
    }

    fn heads(ed: &Editor) -> Vec<usize> {
        ed.selections().unwrap().all().iter().map(|s| s.head.at).collect()
    }

    #[test]
    fn typing_inserts_at_every_cursor() {
        //                   0123456789
        let mut ed = with_cursors("aa bb cc", &[0, 3, 6]);
        ed.session.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xaa Xbb Xcc");
    }

    /// The reason edits run highest-position-first: an insert at 0 shifts
    /// everything after it, so an ascending pass would put the later ones in
    /// the wrong place.
    #[test]
    fn later_cursors_are_not_shifted_by_earlier_edits() {
        let mut ed = with_cursors("....|....|", &[4, 9]);
        ed.session.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('#')));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "....#|....#|");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "  ");
    }

    #[test]
    fn a_multi_cursor_edit_is_one_undo_step() {
        let mut ed = with_cursors("aa bb cc", &[0, 3, 6]);
        ed.session.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        ed.session.mode = Mode::Normal;
        ed.apply(cmd(Action::EnterNormal));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xaa Xbb Xcc");
        ed.apply(cmd(Action::Undo));
        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "aa bb cc",
            "one u, not one per cursor",
        );
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
        assert_eq!(ed.selections().unwrap().len(), 1, "collided cursors are one cursor");
    }

    #[test]
    fn the_primary_cursor_is_what_the_viewport_and_status_line_follow() {
        let ed = with_cursors("one\ntwo\nthree", &[0, 8]);
        assert_eq!(ed.cursor().unwrap().at, ed.selections().unwrap().primary().head.at);
        assert!(ed.cursor_row().unwrap() <= 2);
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xaa Xbb");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "aa bb");
        assert_eq!(ed.selections().unwrap().len(), 2, "both cursors come back");
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
        let sel = ed.selections().unwrap().primary();
        assert_eq!(sel.anchor.at, 0, "the anchor stays where the selection began");
        assert_eq!(sel.head.at, 2);
    }

    #[test]
    fn the_same_key_again_leaves_visual_mode() {
        let mut ed = visual("hello", 0, VisualKind::Char);
        assert_eq!(ed.session.mode, Mode::Visual(VisualKind::Char));
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.selections().unwrap().primary().is_collapsed());
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
        let before = ed.selections().unwrap().primary();
        ed.apply(cmd(Action::SwapEnds));
        let after = ed.selections().unwrap().primary();
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "llo", "both h and e");
        assert_eq!(ed.session.mode, Mode::Normal, "and it drops back to normal");
    }

    #[test]
    fn a_linewise_operator_takes_whole_lines_whatever_the_columns() {
        let mut ed = visual("one\ntwo\nthree", 5, VisualKind::Line);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\nthree");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "hello");
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
        let sel = ed.selections().unwrap().primary();
        assert_eq!((sel.anchor.at, sel.head.at), (4, 6), "the head sits on the last char");

        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "foo  baz");
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
            ed.block_spans().iter().map(|&(s, e)| ed.buffer().unwrap().slice(s, e)).collect();
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
            ed.block_spans().iter().map(|&(s, e)| ed.buffer().unwrap().slice(s, e)).collect();
        assert_eq!(text, vec!["def", "jkl", "pqr"]);
    }

    #[test]
    fn deleting_a_block_cuts_the_same_columns_from_every_row() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "adef\ngjkl\nmpqr");
        assert_eq!(ed.cursor().unwrap().at, 1, "and lands on the top-left corner");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn a_yanked_block_is_a_blockwise_entry_of_its_rows() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        let entry = ed.session.registers.front().unwrap();
        assert_eq!(entry.kind, EntryKind::Blockwise);
        assert_eq!(entry.text, "bc\nhi\nno", "rows joined, no terminator");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), GRID);
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcf\ngh\nmnor");
    }

    #[test]
    fn dollar_takes_every_row_to_its_own_end() {
        let mut ed = block("abcdef\ngh\nmnopqr", 1, 2, 0);
        ed.apply(cmd(Action::Move(Motion::LineEnd)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\ng\nm", "ragged, not a column");
    }

    #[test]
    fn a_motion_after_dollar_gives_the_edge_back_to_the_head() {
        let mut ed = block("abcdef\ngh\nmnopqr", 1, 2, 0);
        ed.apply(cmd(Action::Move(Motion::LineEnd)));
        ed.apply(cmd(Action::Move(Motion::LineStart)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "acdef\ng\nmopqr");
    }

    #[test]
    fn changing_a_block_puts_a_cursor_on_every_row() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Change, sink: Sink::Ring }));
        assert_eq!(ed.session.mode, Mode::Insert);
        assert_eq!(ed.selections().unwrap().len(), 3);
        type_str(&mut ed, "X");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "aXdef\ngXjkl\nmXpqr");
    }

    #[test]
    fn block_insert_puts_a_cursor_at_the_left_edge_of_every_row() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::BlockInsert { append: false }));
        assert_eq!(ed.session.mode, Mode::Insert);
        type_str(&mut ed, "-");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a-bcdef\ng-hijkl\nm-nopqr");
    }

    #[test]
    fn block_insert_skips_a_row_that_does_not_reach_the_block() {
        let mut ed = block("abcdef\ngh\nmnopqr", 3, 2, 1);
        ed.apply(cmd(Action::BlockInsert { append: false }));
        type_str(&mut ed, "-");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abc-def\ngh\nmno-pqr");
    }

    #[test]
    fn block_append_pads_a_short_row_so_the_text_lines_up() {
        let mut ed = block("abcdef\ngh\nmnopqr", 3, 2, 1);
        ed.apply(cmd(Action::BlockInsert { append: true }));
        type_str(&mut ed, "|");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcde|f\ngh   |\nmnopq|r");
    }

    #[test]
    fn block_append_after_dollar_lands_at_each_line_end() {
        let mut ed = block("abcdef\ngh\nmnopqr", 1, 2, 0);
        ed.apply(cmd(Action::Move(Motion::LineEnd)));
        ed.apply(cmd(Action::BlockInsert { append: true }));
        type_str(&mut ed, ";");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdef;\ngh;\nmnopqr;");
    }

    #[test]
    fn swapping_corners_keeps_the_rows_and_swaps_the_columns() {
        let mut ed = block(GRID, 1, 2, 2);
        ed.apply(cmd(Action::SwapCorners));
        let sel = ed.selections().unwrap().primary();
        assert_eq!(
            (ed.buffer().unwrap().row_at(sel.anchor), ed.buffer().unwrap().col_at(sel.anchor)),
            (0, 3)
        );
        assert_eq!(
            (ed.buffer().unwrap().row_at(sel.head), ed.buffer().unwrap().col_at(sel.head)),
            (2, 1)
        );
        let text: Vec<String> =
            ed.block_spans().iter().map(|&(s, e)| ed.buffer().unwrap().slice(s, e)).collect();
        assert_eq!(text, vec!["bcd", "hij", "nop"], "the same rectangle either way round");
    }

    #[test]
    fn r_over_a_block_overwrites_every_character_in_it() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::ReplaceSelection('.')));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a..def\ng..jkl\nm..pqr");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn r_over_a_charwise_selection_spans_lines_without_eating_the_newline() {
        let mut ed = visual("abc\ndef", 1, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::ReplaceSelection('.')));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a..\n..f");
    }

    #[test]
    fn undoing_a_block_delete_leaves_a_cursor_rather_than_a_selection() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        ed.apply(cmd(Action::Undo));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), GRID);
        assert_eq!(ed.selections().unwrap().len(), 1);
        assert!(
            ed.selections().unwrap().primary().is_collapsed(),
            "vim leaves no selection behind an undo, and normal mode cannot act on one"
        );
        assert_eq!(ed.cursor().unwrap().at, 1, "on the start of what came back");
    }

    #[test]
    fn undoing_a_multi_cursor_edit_still_gives_the_cursors_back() {
        let mut ed = with_cursors("one two three", &[0, 4, 8]);
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "X");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xone Xtwo Xthree");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one two three");
        assert_eq!(ed.selections().unwrap().len(), 3, "the cursors are part of what undo restores");
        assert!(ed.selections().unwrap().all().iter().all(|s| s.is_collapsed()));
    }

    #[test]
    fn r_reaches_every_selection_when_there_is_more_than_one() {
        let mut ed = visual("foo bar foo", 0, VisualKind::Char);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(ed.selections().unwrap().len(), 2);
        ed.apply(cmd(Action::ReplaceSelection('.')));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "... bar ...");
        assert_eq!(ed.selections().unwrap().len(), 2, "and the cursors survive it");
    }

    #[test]
    fn a_block_is_single_selection_so_entering_one_drops_the_extra_cursors() {
        let mut ed = editor(GRID);
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        assert_eq!(ed.selections().unwrap().len(), 2);
        ed.apply(cmd(Action::EnterVisual(VisualKind::Block)));
        assert_eq!(ed.selections().unwrap().len(), 1);
    }

    #[test]
    fn pasting_a_block_puts_a_rectangle_back() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        ed.set_cursor(Cursor::at(4)); // row 0, column 4
        ed.apply(cmd(Action::Paste { before: false, count: 1 }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdebcf\nghijkhil\nmnopqnor");
    }

    #[test]
    fn pasting_a_block_pads_short_rows_and_grows_the_buffer() {
        let mut ed = editor("xy");
        ed.session.registers.push(Entry { text: "bc\nhi\nno".into(), kind: EntryKind::Blockwise });
        ed.set_cursor(Cursor::at(1));
        ed.apply(cmd(Action::Paste { before: true, count: 1 }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "xbcy\n hi\n no");
    }

    #[test]
    fn a_repeated_block_operator_cuts_the_same_rectangle_again() {
        let mut ed = block(GRID, 1, 1, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "adef\ngjkl\nmnopqr");

        ed.set_cursor(Cursor::at(11)); // row 2, column 1
        ed.apply(cmd(Action::RepeatChange { count: None }));
        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "adef\ngjkl\nmpqr",
            "one row, two columns"
        );
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "XYcdef");
    }

    #[test]
    fn replace_past_the_end_of_the_line_appends() {
        // Vim does not let it eat the newline.
        let ed = replaced("ab\nnext", "XYZ");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "XYZ\nnext");
    }

    /// The one thing `R` has that overwriting alone does not: Backspace puts
    /// the original characters back. Not testable through the vim differential
    /// harness, which inserts the DEL byte literally.
    #[test]
    fn backspace_in_replace_mode_restores_what_was_overwritten() {
        let mut ed = replaced("abcdef", "XY");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "XYcdef");

        ed.apply(cmd(Action::ReplaceBackspace));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xbcdef", "the b came back");
        ed.apply(cmd(Action::ReplaceBackspace));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdef", "and the a");
    }

    #[test]
    fn backspacing_past_an_appended_char_removes_it_rather_than_restoring() {
        let mut ed = replaced("ab", "XYZ");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "XYZ");
        // The Z was appended past the end, so there is nothing to put back.
        ed.apply(cmd(Action::ReplaceBackspace));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "XY");
    }

    #[test]
    fn leaving_replace_mode_forgets_what_it_overwrote() {
        let mut ed = replaced("abcdef", "XY");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.session.mode, Mode::Normal);
        // Nothing to pop, so this must not put stale characters back.
        ed.apply(cmd(Action::ReplaceBackspace));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "XYcdef");
    }

    #[test]
    fn a_replace_session_is_one_undo_step() {
        let mut ed = replaced("abcdef", "XY");
        ed.apply(cmd(Action::EnterNormal));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdef");
    }

    // ---- multi-cursor ------------------------------------------------------

    #[test]
    fn ctrl_n_puts_a_cursor_on_the_next_occurrence() {
        let mut ed = editor("foo bar foo baz foo");
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(ed.selections().unwrap().len(), 2);
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xfoo\nXfoo\nXfoo");
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
        assert_eq!(ed.selections().unwrap().len(), 1);
        assert!(!ed.session.status.is_empty());
    }

    #[test]
    fn ctrl_alt_down_adds_a_cursor_below_keeping_the_column() {
        let mut ed = editor("hello\nworld\nthere");
        ed.set_cursor(Cursor::at(3));
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        assert_eq!(ed.selections().unwrap().len(), 2);
        assert_eq!(heads(&ed), vec![3, 9], "same column, next row");
    }

    #[test]
    fn adding_a_cursor_past_the_last_line_reports_rather_than_wrapping() {
        let mut ed = editor("only one line");
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        assert_eq!(ed.selections().unwrap().len(), 1);
        assert!(!ed.session.status.is_empty());
    }

    #[test]
    fn a_cursor_below_clamps_to_a_shorter_line() {
        let mut ed = editor("longer line\nab");
        ed.set_cursor(Cursor::at(9));
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        let row1 = ed.buffer().unwrap().rope().line_to_char(1);
        assert_eq!(heads(&ed), vec![9, row1 + 1], "clamped onto the short line");
    }

    #[test]
    fn esc_collapses_to_the_primary_cursor() {
        let mut ed = editor("foo foo foo");
        ed.apply(cmd(Action::AddCursorNextMatch));
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(ed.selections().unwrap().len(), 3);

        ed.apply(cmd(Action::CollapseCursors));
        assert_eq!(ed.selections().unwrap().len(), 1);
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\nb\nc");
    }

    #[test]
    fn ctrl_n_in_visual_mode_selects_the_next_occurrence_of_the_selection() {
        let mut ed = editor("abc abc");
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::AddCursorNextMatch));

        assert_eq!(ed.selections().unwrap().len(), 2);
        assert!(
            ed.selections().unwrap().all().iter().all(|s| !s.is_collapsed()),
            "both are ranges, because visual mode is about ranges",
        );
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), " ");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "bcdef");
        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "cdef");
        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "def", "and again");
    }

    #[test]
    fn dot_replays_a_whole_insert_session_as_one_unit() {
        let mut ed = editor("hello");
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "AB");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "ABhello");

        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "AABBhello");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "achello");

        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "aacchello");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "two three");

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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "tthree", "still the delete");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdef");
        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "bcdef", "the delete, not the undo");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "defghi");
        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "ghi", "three more");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "ef");
    }

    #[test]
    fn dot_with_nothing_recorded_says_so() {
        let mut ed = editor("abc");
        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abc");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "defgh");

        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "gh", "three more characters");
    }

    #[test]
    fn dot_after_a_linewise_visual_delete_repeats_the_line_count() {
        let mut ed = editor("1\n2\n3\n4\n5");
        ed.apply(cmd(Action::EnterVisual(VisualKind::Line)));
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "3\n4\n5");

        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "5", "two more lines");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "aa", "two at a time, three times");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "x x x");
        dot(&mut ed);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "  ");
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
        assert_eq!(ed.cursor_col().unwrap(), 2, "past the B while inserting");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.cursor_col().unwrap(), 1, "back onto the B");
    }

    #[test]
    fn leaving_visual_mode_does_not_step_the_cursor() {
        let mut ed = editor("hello");
        ed.set_cursor(Cursor::at(3));
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.cursor_col().unwrap(), 3, "only insert steps back");
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
        assert_eq!(ed.cursor().unwrap().at, 8);
    }

    #[test]
    fn a_search_wraps_round_the_end() {
        let mut ed = editor("one two");
        ed.set_cursor(Cursor::at(6));
        search_for(&mut ed, "one", true);
        assert_eq!(ed.cursor().unwrap().at, 0);
    }

    #[test]
    fn a_backward_search_goes_the_other_way() {
        let mut ed = editor("one two three");
        ed.set_cursor(Cursor::at(12));
        search_for(&mut ed, "two", false);
        assert_eq!(ed.cursor().unwrap().at, 4);
    }

    #[test]
    fn n_repeats_in_the_direction_the_search_was_typed() {
        let mut ed = editor("a1a2a3");
        ed.set_cursor(Cursor::at(5));
        search_for(&mut ed, "a", false);
        assert_eq!(ed.cursor().unwrap().at, 4, "backward to the third a");
        ed.apply(cmd(Action::Move(Motion::Search { reverse: false })));
        assert_eq!(ed.cursor().unwrap().at, 2, "n keeps going backward");
        ed.apply(cmd(Action::Move(Motion::Search { reverse: true })));
        assert_eq!(ed.cursor().unwrap().at, 4, "N reverses");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "three four", "stops before the match");
    }

    #[test]
    fn smartcase_is_insensitive_until_the_pattern_has_a_capital() {
        // A search finds the *next* match, so both of these start before the
        // only candidate rather than on it.
        let mut ed = editor("bar FOO");
        search_for(&mut ed, "foo", true);
        assert_eq!(ed.cursor().unwrap().at, 4, "an all-lowercase pattern ignores case");

        // Two candidates, differing only in case: a capital in the pattern
        // makes it skip the lowercase one.
        let mut ed = editor("x foo Foo");
        search_for(&mut ed, "Foo", true);
        assert_eq!(ed.cursor().unwrap().at, 6, "a capital makes it case-sensitive");
    }

    #[test]
    fn star_matches_whole_words_only() {
        let mut ed = editor("foo\nfoobar\nfoo");
        ed.apply(cmd(Action::SearchWord { forward: true }));
        assert_eq!(ed.cursor_row().unwrap(), 2, "skipped foobar");
    }

    #[test]
    fn a_pattern_that_is_not_there_reports_and_does_not_move() {
        let mut ed = editor("abc");
        ed.set_cursor(Cursor::at(1));
        search_for(&mut ed, "zzz", true);
        assert_eq!(ed.cursor().unwrap().at, 1);
        assert!(ed.session.status.contains("not found"), "got: {}", ed.session.status);
    }

    #[test]
    fn a_bare_search_repeats_the_last_pattern() {
        let mut ed = editor("a1a2a3");
        search_for(&mut ed, "a", true);
        assert_eq!(ed.cursor().unwrap().at, 2);
        search_for(&mut ed, "", true);
        assert_eq!(ed.cursor().unwrap().at, 4, "the empty pattern reuses the last one");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one two", "the operator went with it");
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
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "XbXc");
        ed.apply(cmd(Action::RepeatChange { count: None }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "Xc");
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
        assert_eq!(ed.cursor_row().unwrap(), 4, "and the cursor was pushed clear of the scrolloff");
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
        assert_eq!(ed.cursor_row().unwrap(), 7);
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
