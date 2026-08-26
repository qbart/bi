//! Editor state and the action dispatch table.
//!
//! [`Action`] is the seam. Today `input.rs` is the only thing that produces
//! actions and the keymap is hardcoded; when a config language shows up, it
//! produces actions too and nothing here changes.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::buffer::{Buffer, BufferId, Cursor};
use crate::clipboard::SystemClipboard;
use crate::cmd_history::History;
use crate::cmdline::CmdLine;
use crate::config::{Config, ConfigSource, Diagnostic, OptionPatch, OptionValue, Options};
use crate::history::Cursors;
use crate::img::Img;
use crate::lsp;
use crate::motion::{Motion, Operator, Target, TextObject};
use crate::picker::{Item, Picker, PickerKind, REGISTER_MIN_LEN};
use crate::range::{Address, Scope, Where};
use crate::region::{Region, Shape};
use crate::registers::{Entry, Registers, Sink};
use crate::selection::{Selection, Selections};
use crate::syntax::Syntax;
use crate::theme::Theme;
use crate::tree::{ClipMode, Clipboard, Kind, Mark, Tree, copy_into, move_into};
use crate::window::{
    Chrome, Content, ContentKind, Dir, Layout, Place, Rect, Side, Text, Window, WindowId,
};

/// What a `:` command with no scope of its own acts on.
///
/// One value per default that exists, named where the command is dispatched,
/// so that "no range means the cursor's line" is a decision written down once
/// rather than four private ones that can drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    /// The word under each cursor. `:case`.
    Words,
    /// The cursor's line. `:s`.
    CursorRow,
    /// Every line. `:retab`.
    File,
    /// The rows the selection covers. `:m`.
    SelectionRows,
}

/// Where a key moves the cursor on the `:` line.
///
/// Four values because four is what a prompt has: one character either way,
/// and the two ends. See `docs/specs/cmdline.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdMove {
    Left,
    Right,
    Home,
    End,
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
    Visual(Shape),
    /// The `:` line being typed, without the leading colon. Its cursor rides
    /// with it — see `docs/specs/cmdline.md`.
    Command(crate::cmdline::CmdLine),
    /// The `/` or `?` line being typed, without the leading key.
    Search {
        query: String,
        forward: bool,
    },
    /// The picker overlay is up. Its state lives in `Editor::picker` — a
    /// `Picker` is far too large to sit inside this enum.
    Pick,
    /// Letters are on screen and the next key picks one. The list lives in
    /// `Session::labels`, beside the picker's and for the same reason.
    Label,
    /// `s` — typing narrows what is matched on screen and a letter jumps to
    /// one. See `docs/specs/find.md`.
    Find,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Replace => "REPLACE",
            Mode::Visual(Shape::Chars) => "VISUAL",
            Mode::Visual(Shape::Lines) => "V-LINE",
            Mode::Visual(Shape::Block) => "V-BLOCK",
            Mode::Command(_) => "COMMAND",
            Mode::Search { .. } => "SEARCH",
            Mode::Pick => "PICK",
            Mode::Label => "LABEL",
            Mode::Find => "FIND",
        }
    }

    /// Whether the cursor may rest one past the last char of a line.
    pub fn allows_eol(&self) -> bool {
        matches!(self, Mode::Insert | Mode::Replace)
    }

    pub fn visual(&self) -> Option<Shape> {
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
    /// `p` / `P`. Reads the front of the ring, or the system clipboard when
    /// the command was spelled `"+p`.
    Paste {
        before: bool,
        count: usize,
        sink: Sink,
    },
    /// `p` / `P` in visual mode — the selection is replaced by the register.
    ///
    /// Its own action rather than a flag on `Paste`: the range comes from the
    /// selection, the edit replaces rather than inserts, and it ends visual
    /// mode. `capture` is the `p`/`P` difference — whether what it displaced
    /// goes on the ring, which is what makes `p` a swap.
    PasteSelection {
        capture: bool,
        count: usize,
        sink: Sink,
    },

    OpenPicker(PickerKind),
    /// A key pressed while the labels are up.
    LabelChar(char),
    LabelCancel,
    /// `S` — a letter at both ends of every scope around the cursor.
    ShowScopes,
    /// `s` — dim the screen and wait for something to look for.
    EnterFind,
    FindChar(char),
    FindBackspace,
    FindCancel,
    PickChar(char),
    PickBackspace,
    PickNext,
    PickPrev,
    PickAccept,
    PickCancel,
    PickToggleShort,

    /// `Ctrl-N` / `Ctrl-P` in insert mode. With the menu open they move its
    /// selection; with it closed, `Ctrl-N` summons one — vim's own key for
    /// exactly this. Handled in `Editor::apply`, never in a view: the menu
    /// is session state. See `docs/specs/complete.md`.
    CompleteNext,
    CompletePrev,
    EnterInsert,
    EnterInsertAfter,
    EnterInsertLineStart,
    EnterInsertLineEnd,
    EnterNormal,
    /// `v` / `V`. The same key again leaves, as in vim.
    EnterVisual(Shape),
    /// A key in a results pane — see `docs/specs/find-in-files.md`.
    Results(ResultsCmd),
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
    /// `<C-x>` — the same match, passed over: the newest cursor moves on to
    /// the one after it instead of a cursor being left behind.
    SkipCursorToNextMatch,
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
    /// `Shift-Up` / `Shift-Down`, and `:m` — move this line, or the selected
    /// block, by `rows` in that direction.
    MoveLines {
        down: bool,
    },
    /// `:m 0`, `:m $`, `:m 12` — move it so it starts at that row.
    MoveLinesTo {
        row: usize,
    },
    /// `ys{motion}{char}` — wrap what the motion covers.
    ///
    /// Spelled with the yank operator's key because yank is the operator that
    /// changes nothing, and neither does the `y` in `ys`. See
    /// `docs/specs/surround.md`.
    Surround {
        target: Target,
        count: usize,
        with: char,
    },
    /// `ds{char}` — remove the innermost pair the cursor is inside.
    Unsurround {
        of: char,
    },
    /// `cs{old}{new}` — one pair into another, in place.
    Resurround {
        of: char,
        with: char,
    },
    /// `S{char}` in visual mode — wrap the selection.
    SurroundSelection {
        with: char,
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
    /// A run of text arriving at once — a terminal's bracketed paste. One
    /// insertion, one undo entry, one reparse, however long it is.
    InsertText(String),
    InsertNewline,
    Backspace,
    /// `Tab` / `Shift-Tab` in insert mode — to the next indent stop, or back
    /// to the previous one. Not an `InsertChar('\t')`, because where the stop
    /// is depends on where on the line the cursor already is.
    InsertIndent {
        right: bool,
    },

    EnterCommandMode,
    /// A `:` line from a binding, rather than one that was typed.
    ///
    /// `run` is what the binding's trailing `<CR>` said. With it, straight
    /// through `run_ex` — the same entry point the command line uses, which is
    /// the only way the two stay in agreement. Without it, the line is
    /// prefilled and left for you to finish, which is how `":e "` asks for a
    /// path and how the tree's `a` and `r` keys have always worked.
    Ex {
        line: String,
        run: bool,
    },
    /// `]]` / `[[` — the next or previous tree-sitter boundary. See
    /// `docs/specs/boundaries.md`.
    BoundaryJump {
        forward: bool,
    },
    CommandChar(char),
    CommandBackspace,
    /// Moving the cursor along the `:` line. Arrows and the shells' `Ctrl-A` /
    /// `Ctrl-E`, because there is no normal mode on a prompt to put motions in
    /// — see `docs/specs/cmdline.md`.
    CommandMove(CmdMove),
    /// `Up` / `Down`: older or newer, out of the command history.
    CommandRecall {
        older: bool,
    },
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
        // A move repeats one row at a time, so `3 Shift-Down` clamps at the
        // bottom the same way three presses would.
        //
        // Visual `>` is here because its count is steps rather than rows — the
        // selection already says which rows — and three steps is the command
        // three times. It works only because it keeps the selection.
        if let Action::OperateSelection { op: Operator::Indent { .. }, .. } = self {
            return true;
        }
        matches!(
            self,
            Action::InsertChar(_) | Action::Undo | Action::Redo | Action::MoveLines { .. }
        )
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
            | Action::PasteSelection { .. }
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
            | Action::Surround { .. }
            | Action::Unsurround { .. }
            | Action::Resurround { .. }
            | Action::SurroundSelection { .. }
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
                | Action::InsertText(_)
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
        Action::Paste { before, sink, .. } => Action::Paste { before, count, sink },
        Action::PasteSelection { capture, sink, .. } => {
            Action::PasteSelection { capture, count, sink }
        }
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
    /// Still to place, the head first. Each carries its own verb, so a mixed
    /// set copies some and moves others in one pass.
    queue: Vec<Mark>,
    into: std::path::PathBuf,
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
    /// A capture waiting on its register name — the text `"n{operator}` took,
    /// held while the `:yname ` prompt is up. Never survives leaving the
    /// prompt: `:yname` consumes it, anything else sends it to the ring.
    pending_named: Option<Entry>,
    /// The world's clipboard, when a frontend has supplied one. `None` is an
    /// embedder that has not, and every test: `"+y` then says so and changes
    /// nothing, which is a diagnostic rather than a panic.
    system: Option<Box<dyn SystemClipboard>>,
    pub picker: Option<Picker>,
    /// The mode the picker was opened from, and the one it gives back.
    ///
    /// A register picked over a selection replaces it, so the visual mode has
    /// to survive the overlay; a history picked over a half-typed `:` line has
    /// to give that line back when you cancel. The whole mode rather than the
    /// visual kind alone, so "the picker returns you where you were" is one
    /// rule rather than one rule and a special case. On the session rather than
    /// in `PickerKind` because the picker is a general overlay and this is the
    /// editor's business, not the widget's.
    pick_from: Option<Mode>,
    /// The `:` lines you have run, for `Ctrl-R`. Beside the registers, and
    /// session state for the same reason: it is not a fact about any buffer,
    /// and it outlives every one of them. See `docs/specs/cmdline-history.md`.
    pub cmd_history: History,
    /// The options `:set` has been given this session, as a layer rather than
    /// as values: they have to be re-applied on top of whatever a file's type
    /// and project ask for, or `:set tab_width 8` would silently do nothing in
    /// the repositories that have an `.editorconfig`. See
    /// `docs/specs/options.md`.
    /// What the last yank read, and until when — see `docs/specs/flash.md`.
    pub flash: Option<Flash>,
    /// What `K` asked and the server answered, until the next command — the
    /// flash's rule. State only; the frontend draws the float. See
    /// `docs/specs/hover.md`.
    pub hover: Option<Hover>,
    /// The completion menu, while one is up. See `docs/specs/complete.md`.
    pub completion: Option<crate::complete::Completion>,
    /// The parameters float, while the cursor is in a call. Unlike `hover`
    /// it survives commands — typing is how it is used — and closes when the
    /// server says the call ended. See `docs/specs/signature.md`.
    pub signature: Option<Signature>,
    /// Buffers written since the last settle, for LSP `didSave` — recorded by
    /// the write paths, drained by `Editor::settle`, the same shape as
    /// `pending_edits` itself. On the session because a write can happen from
    /// a view, which holds no registry.
    pub pending_saves: Vec<BufferId>,
    /// The letters currently on screen, and what they stand for — see
    /// `docs/specs/labels.md`.
    pub labels: Option<Labels>,
    /// What `s` has been given to look for, and what it found — see
    /// `docs/specs/find.md`.
    pub find: Option<Find>,
    /// Every buffer, most recently shown first — what `C-Tab` lists and what
    /// decides which one it opens on. See `docs/specs/buffers.md`.
    mru: Vec<BufferId>,
    pub overrides: OptionPatch,
    /// `[filetype.<name>]` from the config, kept beside the options it patches.
    pub filetypes: std::collections::BTreeMap<String, OptionPatch>,
    pub mode: Mode,
    /// The kind of selection the `:` line interrupted.
    ///
    /// `Mode::Command` *replaces* `Mode::Visual`, so without this the rectangle
    /// flag — which lives in the mode and nowhere else — is destroyed the
    /// moment you press `:`, and a block selection is drawn and acted on as a
    /// plain char range. The selections themselves were never lost; only the
    /// word for what shape they are.
    ///
    /// Only ever read while the mode is `Command` (see [`Session::visual`]),
    /// so a value left behind here cannot paint a rectangle over a later
    /// normal mode.
    interrupted_visual: Option<Shape>,
    /// The last results list that left a window — Enter displaced it, `q`
    /// closed it, a new search replaced it. `:results` puts it back exactly
    /// as it was: selection, prunes and applied marks intact. Nothing here
    /// re-runs the search. See `docs/specs/find-in-files.md`.
    parked_results: Option<Box<crate::results::Results>>,
    /// Where each row of a [`PickerKind::Symbol`] list goes, by the same index
    /// the picker holds its items at.
    ///
    /// Beside the picker rather than inside it: a `picker::Item` is a string,
    /// which is all every other kind needs, and giving one an optional char
    /// offset would put a field on four lists that have no use for it.
    symbol_targets: Vec<usize>,
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
    /// The last `:s` that ran, pattern resolved, for `&` and `:&&`. Beside
    /// `last_search` because it is set in the same breath — see
    /// `docs/specs/substitute.md`.
    pub last_substitute: Option<crate::substitute::Substitute>,
    /// Whether `:ts` has the boundaries on show — a toggle, recomputed each
    /// frame from the live tree. See `docs/specs/boundaries.md`.
    pub ts_marks: bool,
    /// Whether the chrome is off — see `docs/specs/zen.md`. Session state
    /// exactly as `ts_marks` is: a way of looking, not a fact about a file.
    pub zen: bool,
    /// Everything `:set` can change, and everything `[options]` can say.
    ///
    /// One struct rather than a field each so that a new option is one line in
    /// `Options` instead of one field here, one match arm in `set_option` and
    /// one parse rule. `:reload` also needs to replace all of them at once,
    /// which a struct does and a spray of fields does not.
    ///
    /// Session-wide by choice, where vim scopes `'number'` per window. A
    /// gutter numbered in one pane and not in its neighbour makes the same
    /// file read differently depending on where you opened it, and the setting
    /// is a reading preference rather than anything about the view. See
    /// `docs/specs/windows.md`.
    pub options: Options,
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
    /// The directory the session is scoped to — what `bi .` was pointed at,
    /// and what every tree opens on afterwards.
    ///
    /// Session state rather than the tree's, because a tree is destroyed every
    /// time a file displaces it and the scope you chose must outlive that.
    /// Deriving it from the open file instead is what made opening `pkg/a.rs`
    /// silently re-root at `pkg`: the root is something you set, with `+`, `-`
    /// or a directory named outright, and nothing else may move it. See
    /// `docs/specs/tree.md`.
    pub tree_root: Option<PathBuf>,
    pub status: String,
    pub quit: bool,
}

impl Session {
    /// What kind of selection is on screen, if any.
    ///
    /// Not `Mode::visual()`, because two modes can have one: the `:` line
    /// inherits the selection it interrupted, so a rectangle is still a
    /// rectangle while you type the command that is about to act on it. Every
    /// other mode answers `None` whatever [`Session::interrupted_visual`]
    /// happens to hold, which is what keeps a stale value from painting.
    pub fn visual(&self) -> Option<Shape> {
        match &self.mode {
            Mode::Visual(kind) => Some(*kind),
            Mode::Command(_) => self.interrupted_visual,
            _ => None,
        }
    }

    /// Where an operator's text goes.
    ///
    /// The one place a `Sink` is spent, so a new register is a new arm here
    /// rather than a fourth copy of this decision at a third call site.
    fn capture(&mut self, entry: Entry, sink: Sink) {
        match sink {
            Sink::Ring => self.registers.push(entry),
            // Nothing ever reaches the black hole, which is the point of it.
            Sink::BlackHole => {}
            // Held, not stored: the name is asked for once the command is
            // done — `Editor::apply` opens the `:yname ` prompt when it sees
            // the capture waiting, because opening it here would be undone by
            // the mode changes the rest of the command still makes.
            Sink::Named => self.pending_named = Some(entry),
            Sink::System => {
                let wrote = match &self.system {
                    Some(clipboard) => clipboard.set(&entry.text).map_err(|e| format!("{e:#}")),
                    None => Err("no system clipboard".into()),
                };
                if let Err(message) = wrote {
                    self.status = message;
                }
            }
        }
    }

    /// What `"+p` puts, or `None` with a message saying why not.
    ///
    /// The kind is read off the text, because the clipboard carries none: text
    /// ending in a newline came from whole lines and goes back as whole lines,
    /// which is the same rule vim applies to a register it did not fill.
    fn clipboard_entry(&mut self) -> Option<Entry> {
        let got = match &self.system {
            Some(clipboard) => clipboard.get().map_err(|e| format!("{e:#}")),
            None => Err("no system clipboard".into()),
        };
        let text = match got {
            Ok(Some(text)) if !text.is_empty() => text,
            // Not an error: many terminals refuse to read the clipboard back,
            // and an empty clipboard is an ordinary state.
            Ok(_) => {
                self.status = "the clipboard is empty, or the terminal would not say".into();
                return None;
            }
            Err(message) => {
                self.status = message;
                return None;
            }
        };
        let kind = if text.ends_with('\n') { Shape::Lines } else { Shape::Chars };
        Some(Entry { text, kind })
    }
}

/// A jump being aimed: what has been typed, and every match of it on screen.
///
/// The matches are kept rather than recomputed at draw time, because the
/// letters were assigned against *this* list and a list that had moved on
/// would put them on the wrong words.
pub struct Find {
    pub query: String,
    pub matches: Vec<(usize, usize)>,
}

/// The letters on screen and what they name.
///
/// `typed` accumulates, so a two-character label is two presses with no
/// timeout anywhere — the same rule the keymap's sequences follow.
pub struct Labels {
    pub typed: String,
    pub targets: Vec<(String, LabelTarget)>,
}

/// What pressing a label does.
///
/// An enum rather than a closure, because the editor has to be able to say
/// what a label means without having been the thing that made it — and because
/// `s` and `S` will each add an arm here rather than a mode of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelTarget {
    /// `Ctrl-W f` — go to that window.
    Window(WindowId),
    /// `s` — go to that char offset.
    Position(usize),
    /// `S` — select that char range.
    Scope(usize, usize),
}

impl Labels {
    /// What the typed prefix has reached: a target, or nothing yet, or the
    /// knowledge that nothing can match.
    fn resolve(&self) -> Resolution {
        if let Some((_, target)) = self.targets.iter().find(|(label, _)| *label == self.typed) {
            return Resolution::Hit(*target);
        }
        match self.targets.iter().any(|(label, _)| label.starts_with(&self.typed)) {
            true => Resolution::Pending,
            false => Resolution::Miss,
        }
    }
}

enum Resolution {
    Hit(LabelTarget),
    Pending,
    Miss,
}

/// What a yank lit up, and the moment the light goes out.
///
/// One buffer, because a yank happened in one; several ranges, because a
/// command can yank at several cursors and a rectangle is one span per row.
pub struct Flash {
    pub buffer: BufferId,
    pub ranges: Vec<std::ops::Range<usize>>,
    pub until: std::time::Instant,
}

/// An open buffer, and what belongs to it rather than to a window.
struct BufferEntry {
    id: BufferId,
    buffer: Buffer,
    /// The parse tree, when the file's extension has a grammar.
    ///
    /// Beside the buffer rather than on it, because `pending_edits` has more
    /// than one consumer — tree-sitter and LSP `didChange` — and
    /// whoever drains it destroys it for the others. Putting the tree on
    /// `Buffer` would move the drain inside the buffer and break that.
    syntax: Option<Syntax>,
    /// What kind of file this is — `rust`, `make` — or `None` when nothing
    /// claims the name. The key the option layers and the grammar are both
    /// chosen by; see `crate::syntax::filetype`.
    filetype: Option<&'static str>,
    /// Where this buffer stands with LSP — beside the buffer for the same
    /// reason `syntax` is: per-document derived state, fed at the one drain
    /// point. Resolved lazily in [`Editor::settle`]. See `docs/specs/lsp.md`.
    lsp: lsp::Attach,
    /// The options in force here: the session's, with whatever this file's
    /// type and project ask for laid over them.
    ///
    /// Recomputed rather than patched in place whenever a layer under it moves
    /// — see `docs/specs/options.md`.
    options: Options,
    /// Where the last window to leave this buffer was looking.
    ///
    /// Without it, cycling forward and back through three files loses your
    /// place in all of them, which makes buffer cycling something you use
    /// once. When two windows show one buffer, the last to leave is what this
    /// remembers — there is no better answer, and it costs nothing to say
    /// which one wins.
    last: Cursors,
    /// Where this buffer stands with git: the index's copy of the file and
    /// the diff against it. `None` until a loader is installed and has
    /// something to say — no repository, an untracked file, an embedder that
    /// never calls [`Editor::set_git_baseline`]. See `docs/specs/git-signs.md`.
    git: Option<GitState>,
}

/// The baseline and the diff against it, cached beside the buffer the same
/// way the parse tree is: derived state, refreshed at the drain.
struct GitState {
    baseline: String,
    /// `Buffer::edits` when `diff` was computed — moved means stale.
    seen: u64,
    diff: crate::git::Diff,
}

impl BufferEntry {
    /// Options are left at the defaults here and resolved by
    /// `Editor::resolve_options`, which is the only thing that can see the
    /// layers under them.
    fn new(id: BufferId, buffer: Buffer) -> Self {
        Self {
            id,
            filetype: filetype_of(&buffer),
            syntax: syntax_for(&buffer, &Options::default()),
            buffer,
            options: Options::default(),
            last: Vec::new(),
            lsp: lsp::Attach::Unresolved,
            git: None,
        }
    }
}

/// An identifier char, for the completion word — the same class `w` calls a
/// word char.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Markdown to hover lines, minimally and honestly: fence lines drop and tag
/// what sits between them as code, a horizontal rule becomes [`HoverLine::Rule`],
/// everything else passes through untouched — stripping emphasis loses
/// information and rendering it is a project. Blank edges go; a float has no
/// room for air.
fn hover_lines(markdown: &str) -> Vec<HoverLine> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        out.push(match () {
            _ if in_fence => HoverLine::Code(line.to_string()),
            _ if trimmed == "---" || trimmed == "***" => HoverLine::Rule,
            _ => HoverLine::Text(line.to_string()),
        });
    }
    let blank = |l: &HoverLine| matches!(l, HoverLine::Text(t) if t.trim().is_empty());
    while out.first().is_some_and(blank) {
        out.remove(0);
    }
    while out.last().is_some_and(blank) {
        out.pop();
    }
    out
}

/// The options in force for a file of this type: the session's, with the
/// layers a file gets on top of them.
///
/// A free function because it is asked once per open buffer while the buffer
/// list is borrowed mutably, and because it is the whole of the resolution
/// order in one place — bi's defaults and your config are already inside
/// `session.options`, and what follows is the file's own.
fn resolve_options(
    session: &Session,
    filetype: Option<&'static str>,
    path: Option<&std::path::Path>,
) -> Options {
    let mut options = session.options.clone();
    if let Some(filetype) = filetype {
        crate::config::filetype_defaults(filetype).apply_to(&mut options);
        if let Some(patch) = session.filetypes.get(filetype) {
            patch.apply_to(&mut options);
        }
    }
    // What the project has already agreed, over what the language asks for: a
    // repository with an `.editorconfig` means it. See
    // `docs/specs/editorconfig.md`.
    if let Some(path) = path {
        crate::editorconfig::patch_for(path).apply_to(&mut options);
    }
    // Last, so what you typed this session outranks all of it — an option that
    // silently did nothing in exactly the repositories that have their act
    // together would be worse than no option at all.
    session.overrides.apply_to(&mut options);
    options
}

/// A vertical line down each level of indentation, on every visible row.
///
/// A blank row shows the guides of the *smaller* of its nearest non-blank
/// neighbours, which is what makes a blank line inside a block keep the block's
/// guides while one between two blocks shows none. The scan for those
/// neighbours stops at the first non-blank line in each direction, so it is
/// bounded by how blank the file actually is.
/// The block the cursor is in, named after the line that opened it, hung off
/// the line that closes it.
///
/// `Eol` rather than `Inline`: past the end of the row is the one place
/// virtual text costs nothing, so the search highlight, the selection and the
/// block arithmetic all go on seeing the row as the file says it is. See
/// `docs/specs/tree-sitter-context.md`.
fn context_marks(
    buffer: &Buffer,
    syntax: &Syntax,
    text: &crate::window::Text,
    options: &Options,
    theme: &Theme,
    rows: std::ops::Range<usize>,
    out: &mut Vec<crate::decoration::Decoration>,
) {
    // No line comment, no annotation. Lending CSS or JSON a `//` writes
    // something that reads as a mistake in the file rather than a note about
    // it.
    let Some(marker) = crate::syntax::line_comment(syntax.filetype()) else { return };
    let byte = buffer.rope().char_to_byte(text.selections.cursor().at);
    let found = crate::context::contexts(
        syntax,
        buffer.rope(),
        byte,
        options.context_depth,
        options.context_min_lines,
    );
    for context in found {
        if !rows.contains(&context.closes) {
            continue;
        }
        out.push(crate::decoration::Decoration::Eol {
            row: context.closes,
            text: format!(" {marker} {}", context.opener),
            style: theme.ui.context,
        });
    }
}

/// The blocks the cursor is inside whose opening lines have scrolled off,
/// drawn over the top rows of the pane.
///
/// `Overlay` rather than a row of its own: a decoration cannot change which
/// rows exist, and a header that added one would make every piece of
/// row-to-screen-line arithmetic learn about it. Covering the top row costs a
/// line of the file and no new concept.
///
/// See `docs/specs/tree-sitter-context.md`.
#[allow(clippy::too_many_arguments)]
fn context_header(
    buffer: &Buffer,
    syntax: &Syntax,
    text: &crate::window::Text,
    width: usize,
    options: &Options,
    theme: &Theme,
    rows: std::ops::Range<usize>,
    out: &mut Vec<crate::decoration::Decoration>,
) {
    let depth = options.context_header_depth;
    // A pane the frontend has never sized has no width to fill, and a header
    // is a full-width bar or it is nothing.
    let area = width.saturating_sub(options.gutter_width(buffer.line_count()));
    if depth == 0 || area == 0 {
        return;
    }
    let scroll = text.scroll;
    let cursor_row = buffer.row_at(text.selections.cursor());
    let byte = buffer.rope().char_to_byte(text.selections.cursor().at);

    // The whole chain, then the ones that have scrolled off, outermost first —
    // a header repeating a line three rows below it says nothing and costs a
    // row of code to say it.
    let found = crate::context::contexts(
        syntax,
        buffer.rope(),
        byte,
        usize::MAX,
        options.context_min_lines,
    );
    let lines = found.iter().rev().filter(|c| c.opens < scroll).take(depth);

    for (i, context) in lines.enumerate() {
        let row = scroll + i;
        // Never over the row the cursor is on: scrolling up with `k` puts the
        // cursor on the top row, and a header there hides the line being
        // edited. Everything below this row is the cursor's or past it.
        if row >= cursor_row {
            break;
        }
        if !rows.contains(&row) {
            continue;
        }
        // The row as it is written, indentation and all, so a header two deep
        // reads as the nesting it describes. Padded to the pane, because a bar
        // that stops at the last character is not a bar.
        let mut header = crate::indent::expand_tabs(&buffer.line(context.opens), options.tab_width);
        header.truncate(header.char_indices().nth(area).map_or(header.len(), |(i, _)| i));
        let pad = area - header.chars().count();
        header.push_str(&" ".repeat(pad));

        out.push(crate::decoration::Decoration::Overlay {
            row,
            col: 0,
            text: header,
            style: theme.ui.context_header,
            layer: crate::decoration::Layer::Over,
        });
    }
}

fn indent_guides(
    buffer: &Buffer,
    options: &Options,
    theme: &Theme,
    rows: std::ops::Range<usize>,
    out: &mut Vec<crate::decoration::Decoration>,
) {
    use crate::decoration::{Decoration, Layer};

    let indent = options.indent();
    let step = indent.step();
    let style = theme.ui.indent_guide;
    let total = buffer.line_count();

    for row in rows.start..rows.end.min(total) {
        let width = match buffer.is_blank_row(row) {
            false => buffer.indent_width(row, indent.tab_width),
            true => {
                let above = (0..row)
                    .rev()
                    .find(|&r| !buffer.is_blank_row(r))
                    .map(|r| buffer.indent_width(r, indent.tab_width));
                let below = (row + 1..total)
                    .find(|&r| !buffer.is_blank_row(r))
                    .map(|r| buffer.indent_width(r, indent.tab_width));
                // The end of a block shows no guides rather than the guides of
                // the block that ended: `min` is what says so, and a blank line
                // with nothing on one side of it has no block to belong to.
                match (above, below) {
                    (Some(above), Some(below)) => above.min(below),
                    _ => 0,
                }
            }
        };
        for col in crate::indent::guide_columns(width, step) {
            out.push(Decoration::Overlay {
                row,
                col,
                text: GUIDE.to_string(),
                style,
                layer: Layer::Under,
            });
        }
    }
}

/// Every blank on screen, made visible — what `:whitespace` is.
///
/// A debugging mode, so it is literal: every space is marked, not just the
/// leading or the trailing ones. The question it answers is "what is actually
/// in this line", and an answer that had already decided which blanks were
/// interesting would not be one.
///
/// `Under` the selection, like the guides: selecting a line must still look
/// like selecting a line.
fn whitespace(
    buffer: &Buffer,
    options: &Options,
    theme: &Theme,
    rows: std::ops::Range<usize>,
    out: &mut Vec<crate::decoration::Decoration>,
) {
    use crate::decoration::{Decoration, Layer};

    let tab_width = options.tab_width.max(1);
    let style = theme.ui.whitespace;

    for row in rows.start..rows.end.min(buffer.line_count()) {
        let line = buffer.line(row);
        // Walked once, carrying the column, rather than asking `display_col`
        // per character: a tab is only as wide as where it starts, so the
        // column has to be accumulated anyway.
        let mut col = 0;
        for ch in line.chars() {
            let text = match ch {
                ' ' => Some(WS_SPACE),
                '\t' => Some(WS_TAB),
                // The one that earns its own glyph. A non-breaking space
                // pasted out of a document looks exactly like a space and is
                // not one, and every other way of finding it is a search for a
                // character you cannot type.
                '\u{a0}' => Some(WS_NBSP),
                _ => None,
            };
            if let Some(text) = text {
                out.push(Decoration::Overlay {
                    row,
                    col,
                    text: text.to_string(),
                    style,
                    layer: Layer::Under,
                });
            }
            col += match ch {
                '\t' => tab_width - (col % tab_width),
                _ => 1,
            };
        }
        // Only where there is one. The last row of a file that does not end in
        // a newline gets no pilcrow, and that absence is the report.
        if buffer.has_newline(row) {
            out.push(Decoration::Eol { row, text: WS_EOL.to_string(), style });
        }
    }
}

/// `TODO:` and its friends, wherever they appear.
///
/// Not restricted to comments, and that is a decision rather than an omission:
/// bi has the parse tree and could check, at the cost of a `TODO:` in a
/// Markdown list, a YAML file or any file whose grammar bi does not ship
/// going quiet — which is exactly where people write them. See
/// `docs/specs/todo-comments.md`.
fn todo_comments(
    buffer: &Buffer,
    theme: &Theme,
    rows: std::ops::Range<usize>,
    out: &mut Vec<crate::decoration::Decoration>,
) {
    use crate::decoration::{Decoration, Layer};
    use crate::todo::Tag;

    let ui = &theme.ui;
    for row in rows.start..rows.end.min(buffer.line_count()) {
        let line = buffer.line(row);
        let start = buffer.rope().line_to_char(row);
        for (range, tag) in crate::todo::tags(&line) {
            let style = match tag {
                Tag::Fix => ui.todo_fix,
                Tag::Todo => ui.todo_todo,
                Tag::Warn => ui.todo_warn,
                Tag::Perf => ui.todo_perf,
                Tag::Note => ui.todo_note,
            };
            out.push(Decoration::Repaint {
                range: (start + range.start)..(start + range.end),
                style,
                // Under, so a selected line still reads as selected.
                layer: Layer::Under,
            });
        }
    }
}

/// What `s` puts on screen: the dimming, the matches, and the letters.
fn find_decorations(
    buffer: &Buffer,
    find: &Find,
    labels: Option<&Labels>,
    theme: &Theme,
    options: &Options,
    rows: &std::ops::Range<usize>,
    out: &mut Vec<crate::decoration::Decoration>,
) {
    use crate::decoration::{Decoration, Layer};

    let rope = buffer.rope();
    let last = rows.end.min(buffer.line_count());
    let from = rope.line_to_char(rows.start.min(rope.len_lines().saturating_sub(1)));
    let to = match last >= buffer.line_count() {
        true => rope.len_chars(),
        false => rope.line_to_char(last),
    };
    out.push(Decoration::Repaint { range: from..to, style: theme.ui.dim, layer: Layer::Under });
    for &(start, end) in &find.matches {
        out.push(Decoration::Repaint {
            range: start..end,
            style: theme.ui.search,
            layer: Layer::Under,
        });
    }

    let Some(labels) = labels else { return };
    for (label, target) in &labels.targets {
        let LabelTarget::Position(start) = target else { continue };
        if !label.starts_with(&labels.typed) {
            continue;
        }
        // Drawn *after* the match it belongs to and between the cells rather
        // than over them — you aim at the word and land on its first
        // character, and the word after it is still a word you can read.
        let Some(&(_, end)) = find.matches.iter().find(|(at, _)| at == start) else { continue };
        let row = buffer.row_at(Cursor::at(end.saturating_sub(1)));
        let line = buffer.line(row);
        let col =
            crate::indent::display_col(&line, end - rope.line_to_char(row), options.tab_width);
        out.push(Decoration::Inline { row, col, text: label.clone(), style: theme.ui.label });
    }
}

/// Every boundary as a char position, sorted and deduplicated — the stops
/// `]]` walks and `:ts` paints. A range contributes its first character and
/// its last, which for a one-character node is one stop, not two.
fn boundary_positions(syntax: &Syntax, rope: &ropey::Rope) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for range in syntax.boundaries() {
        let start = rope.byte_to_char(range.start);
        let end = rope.byte_to_char(range.end);
        out.push(start);
        if end > start + 1 {
            out.push(end - 1);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// What `:ts` puts on screen: the dim, and a mark on every boundary — shaped
/// exactly as `find_decorations` shapes `s`'s. See `docs/specs/boundaries.md`.
fn boundary_marks(
    buffer: &Buffer,
    syntax: &Syntax,
    theme: &Theme,
    rows: &std::ops::Range<usize>,
    out: &mut Vec<crate::decoration::Decoration>,
) {
    use crate::decoration::{Decoration, Layer};

    let rope = buffer.rope();
    let last = rows.end.min(buffer.line_count());
    let from = rope.line_to_char(rows.start.min(rope.len_lines().saturating_sub(1)));
    let to = match last >= buffer.line_count() {
        true => rope.len_chars(),
        false => rope.line_to_char(last),
    };
    out.push(Decoration::Repaint { range: from..to, style: theme.ui.dim, layer: Layer::Under });
    for at in boundary_positions(syntax, rope) {
        if at < from || at >= to {
            continue;
        }
        out.push(Decoration::Repaint {
            range: at..at + 1,
            style: theme.ui.search,
            layer: Layer::Under,
        });
    }
}

/// Colour literals, drawn in the colour they name.
///
/// The style is an exact `Rgb` pair rather than anything a theme could have
/// named, which is the case decorations carry a resolved style for.
fn color_swatches(
    buffer: &Buffer,
    rows: std::ops::Range<usize>,
    out: &mut Vec<crate::decoration::Decoration>,
) {
    use crate::decoration::{Decoration, Layer};
    use crate::theme::{Color, Style};

    for row in rows.start..rows.end.min(buffer.line_count()) {
        let line = buffer.line(row);
        let start = buffer.rope().line_to_char(row);
        for swatch in crate::colors::swatches(&line) {
            let (r, g, b) = swatch.rgb;
            let (fr, fg_, fb) = crate::colors::readable_on(swatch.rgb);
            out.push(Decoration::Repaint {
                range: (start + swatch.range.start)..(start + swatch.range.end),
                style: Style {
                    fg: Some(Color::Rgb(fr, fg_, fb)),
                    bg: Some(Color::Rgb(r, g, b)),
                    ..Style::default()
                },
                layer: Layer::Under,
            });
        }
    }
}

/// The character a guide is drawn with.
///
/// Not an option yet, and it is the obvious next one if a font somewhere
/// cannot draw it — vim spells the same idea `listchars`, which is a whole
/// grammar for a handful of characters and is not worth copying for one.
const GUIDE: &str = "\u{2502}";

/// What `:whitespace` draws over each kind of blank.
///
/// Fixed, for the same reason [`GUIDE`] is: vim spells this `listchars`, a
/// grammar for a handful of characters, and four constants are not worth one.
/// The day a font cannot draw `·` is the day they become options.
///
/// A tab gets its arrow at its *first* column and leaves the rest of the
/// expansion blank, so a line's width is what it was — the point is to see
/// where the tab starts, and filling its span would make one tab look like
/// four spaces, which is the confusion being cleared up.
const WS_SPACE: &str = "\u{b7}"; // ·
const WS_TAB: &str = "\u{2192}"; // →
const WS_EOL: &str = "\u{b6}"; // ¶
const WS_NBSP: &str = "\u{2423}"; // ␣

/// The file type of a buffer, by the same whole-name-then-extension rule the
/// grammar is chosen by.
fn filetype_of(buffer: &Buffer) -> Option<&'static str> {
    let path = buffer.path.as_ref()?;
    crate::syntax::filetype(path.file_name()?.to_str()?)
}

/// A parsed `:` line.
///
/// The split that matters is which arms need a rope: those are the last four,
/// and they are the only ones that cannot run in a window holding a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExLine {
    Window(WindowCmd),
    Buffer(BufferCmd),
    /// `:e <path>`. Bare `:e` is [`ExLine::Revert`], which is a different job
    /// — and is not `:reload`, which is the config.
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
    /// `:whitespace`, `:whitespace on|off` — show the blanks.
    ///
    /// `None` is the bare form, which flips whatever is in force where the
    /// cursor is. A toggle rather than only `:set whitespace true` because this
    /// is a thing you turn on to look at something and off again once you have
    /// seen it, and typing the value both times is typing the wrong one.
    Whitespace(Option<bool>),
    Set(String),
    /// `:yname <register>` — stores the capture waiting on a name. Typed by
    /// the prompt `"n` prefills far more often than by hand. With a range or
    /// a selection it is a scoped yank instead: the region goes straight into
    /// the named space, no prompt — the name is already on the line.
    Name {
        scope: Option<Scope>,
        name: String,
    },
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
    /// `:m +3`, `:2,5m 0`, `:m $`.
    ///
    /// `lines` is what the range in front of the command said, and `None` is
    /// what it says when nobody wrote one — where `:m`'s own default lives,
    /// which is the selection. See `docs/specs/ranges.md`.
    Move {
        scope: Option<Scope>,
        to: Address,
    },
    /// `:%s/old/new/g`. `scope` is `None` when nobody wrote one, and `:s`'s
    /// own default is the cursor's line — see `docs/specs/substitute.md`.
    Substitute {
        scope: Option<Scope>,
        how: crate::substitute::Substitute,
    },
    /// `:&` and `:&&` — the last substitute again, flags and all. One meaning
    /// under two spellings, deliberately: vim's flag-dropping `:&` is the
    /// gotcha bi declines. See `docs/specs/substitute.md`.
    SubstituteRepeat {
        scope: Option<Scope>,
    },
    /// `:[range]g/pattern/cmd` — `cmd` on every matching line; `invert` is
    /// `:v` and `:g!`, the lines that do *not* match. The sub-command stays a
    /// string until it runs, so there is exactly one ex grammar. See
    /// `docs/specs/global.md`.
    Global {
        scope: Option<Scope>,
        invert: bool,
        pattern: String,
        cmd: String,
    },
    /// `:[range]normal {keys}` — the keys replayed through the same machinery
    /// a frontend feeds, once per row of the range, or once at the cursor.
    Normal {
        scope: Option<Scope>,
        keys: String,
    },
    /// `:[range]d` — the rows, gone. The cursor's line by default. Short name
    /// only: `:delete` keeps meaning a *path*, and the two cannot be mistyped
    /// into each other because `:delete` demands an argument and `:d` refuses
    /// one. See `docs/specs/global.md`.
    DeleteLines {
        scope: Option<Scope>,
    },
    /// `:[scope]sort [flags]` — order the rows, the whole file when nothing
    /// narrows it. See `docs/specs/sort.md`.
    Sort {
        scope: Option<Scope>,
        how: crate::sort::Sort,
    },
    /// `:[scope]case snake` — respell what is named, or the word under each
    /// cursor when nothing is.
    ///
    /// `'v` is the selection itself, columns and all; an address names rows.
    /// See [`View::recase`] and `docs/specs/regions.md`.
    Case {
        scope: Option<Scope>,
        style: crate::case::Style,
    },
    /// `:retab` — rewrite the indentation to the options in force.
    ///
    /// `None` is the whole file, which is what "convert this file" means and
    /// what vim's `:retab` does. Unlike `:s`, whose no-range default is the
    /// cursor's line: a one-line substitute is a thing people want, and a
    /// one-line retab is not.
    Retab(Option<Scope>),
    /// `:alt` — the other file: the test beside the implementation, the
    /// header beside the source.
    Alternate,
    /// `:symbols` — the declarations in this file, to jump to one.
    Symbols,
    /// `:themes` — every built-in theme, to `:set theme` to. See
    /// `docs/specs/theme.md`.
    Themes,
    /// `:lsp` — where this buffer stands with its language server; `:lsp
    /// restart` and `:lsp stop` manage the instance. See `docs/specs/lsp.md`.
    Lsp(LspCmd),
    /// `:definition` — `gd`. See `docs/specs/lsp-requests.md`.
    Definition,
    /// `:decl` — the declaration: the header's side of the question, where
    /// the languages split the two.
    Declaration,
    /// `:impl` — the implementations: trait impls, overrides, the source
    /// for a header.
    Implementation,
    /// `:peek` — the definition in a fresh vertical split, focus on it. See
    /// `docs/specs/lsp-requests.md`.
    Peek,
    /// `:ts` — toggle the boundary marks. See `docs/specs/boundaries.md`.
    TsMarks,
    /// `:tssplit` — the bracketed list around the cursor, one element per
    /// line. See `docs/specs/splitjoin.md`.
    TsSplit,
    /// `:tsjoin` — the same list back onto one line.
    TsJoin,
    /// `:zen` — the chrome off: gutter, numbers, status rows. The command
    /// line stays. See `docs/specs/zen.md`.
    Zen,
    /// `:diags` — every open buffer's diagnostics, as a `Results` pane. See
    /// `docs/specs/diagnostics.md`.
    Diags,
    /// `:references` — `gr`, into a `Results` pane.
    References,
    /// `:format` — the whole file, by the server, as one undo step.
    Format,
    /// `:dnext` / `:dprev` — `]d` / `[d`, wrapping. See
    /// `docs/specs/diagnostics.md`.
    DiagnosticJump {
        forward: bool,
    },
    /// `:hover` — `K`, a float at the cursor. See `docs/specs/hover.md`.
    Hover,
    /// `:resize 30`, `:resize +3,-3`, `:resize 1:2` — see
    /// `docs/specs/resize.md`.
    Resize(crate::resize::Resize),
    /// `:find <pattern>` — every match under the project root, in a pane.
    /// `:find~` reads the pattern as a regex. A first word ending in `/` that
    /// names a directory under the root is a scope, which is the editor's to
    /// resolve — parsing cannot ask the filesystem.
    Find {
        pattern: String,
        regex: bool,
    },
    /// `:replace /old/new/` — search and offer, `//new/` over the pane's own
    /// matches. Raw here for the same reason `Find`'s scope is: the argument
    /// cannot be split from a possible leading scope without the filesystem.
    Replace {
        arg: String,
        regex: bool,
    },
    /// `:results` — the last results list that left a window, put back.
    ResultsPane,
    Unknown(String),
    /// Parsed, but cannot run — carrying its own message, already phrased.
    Error(String),

    Write(String),
    WriteQuit(String),
    /// Bare `:e` — re-read this file from disk.
    Revert {
        force: bool,
    },
    /// `:42`, `:$`, `:%` — a range and no command, which goes to its last
    /// line. A special case in the parser once; now the general rule falling
    /// out. See `docs/specs/ranges.md`.
    Goto(Address),
    /// `:reload` — the config, not the buffer. See [`ExLine::Revert`].
    ReloadConfig,
}

/// `:m`'s argument: one address, and nothing after it.
///
/// The same language the range in front of the command is written in
/// (`docs/specs/ranges.md`), which is the point of having taken it out of
/// here: `:m .+1`, `:m -2`, `:m $` and `:m 12` are four spellings this file no
/// longer knows the rules for. Trailing anything is refused, so `:m 3 4` is a
/// message rather than a silent `:m 3`.
fn parse_move(arg: &str) -> Option<Address> {
    let (scope, rest) = crate::range::parse(arg.trim());
    // One address, not a span and not the selection: `:m 2,5` names two lines
    // to land after, and `:m 'v` names no line at all.
    let Some(Scope::Lines(range)) = scope else { return None };
    (rest.is_empty() && range.first == range.last).then_some(range.first)
}

/// `s/a/b/g` as `("s", "/a/b/g")` — the delimiter vim lets touch the name.
///
/// Without it `:%s/2024/2025/g` is a command called `s/2024/2025/g`, and the
/// message says so instead of substituting. The name ends at the first
/// character that cannot be part of one, which is what keeps `:set`, `:sp` and
/// `:split` themselves: their next character is a letter, and a letter is
/// never a delimiter. See `docs/specs/substitute.md`.
fn split_glued_substitute(cmd: &str) -> Option<(&str, &str)> {
    let rest = cmd.strip_prefix("substitute").or_else(|| cmd.strip_prefix('s'))?;
    let delimited = rest.starts_with(crate::substitute::is_delimiter);
    delimited.then(|| cmd.split_at(cmd.len() - rest.len()))
}

/// `replace/a/b/` as `("replace", "/a/b/")` — the same glue `:s` gets, for
/// the same fingers. `replace~` first, because `replace` is its prefix.
fn split_glued_replace(cmd: &str) -> Option<(&str, &str)> {
    let rest = cmd.strip_prefix("replace~").or_else(|| cmd.strip_prefix("replace"))?;
    let delimited = rest.starts_with(crate::substitute::is_delimiter);
    delimited.then(|| cmd.split_at(cmd.len() - rest.len()))
}

/// `:g/pattern/cmd`, `:v/pattern/cmd` and their long names, read off the
/// whole line — *before* the whitespace split, because a pattern may contain
/// spaces and the generic glue below would rejoin around them.
///
/// `None` when the line is not a global at all — a name that is not one of
/// the four, or one of them with no delimiter after it (`:vs`, `:vnew` and
/// any future `g…` command all keep working, because a letter is never a
/// delimiter).
fn parse_global(line: &str, scope: Option<Scope>) -> Option<ExLine> {
    let rest = line
        .strip_prefix("global")
        .or_else(|| line.strip_prefix("vglobal"))
        .or_else(|| line.strip_prefix('g'))
        .or_else(|| line.strip_prefix('v'))?;
    // `:v` is `:g!` under vim's other name; both spellings of "does not
    // match" land on the same flag.
    let invert = line.starts_with('v') || rest.starts_with('!');
    let rest = rest.strip_prefix('!').unwrap_or(rest);
    // `:g /foo/d` — the space vim allows before the pattern.
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Some(ExLine::Error("global what? `:g/pattern/cmd`".into()));
    }
    let mut chars = rest.chars();
    let delim = chars.next().filter(|c| crate::substitute::is_delimiter(*c))?;
    let (pattern, cmd) = crate::substitute::take_field(chars.as_str(), delim);
    let cmd = cmd.unwrap_or("").trim().to_string();
    Some(ExLine::Global { scope, invert, pattern, cmd })
}

/// `m+1` as `("m", "+1")` — the address vim lets touch the command name.
///
/// `:m` is the only command that needs this, and it needs it because it is the
/// only one whose argument begins with a character no command name contains.
/// Without it `:m+1` — which is how every vimrc in the world writes it — is a
/// command called `m+1`, and the message says so instead of moving the line.
fn split_glued_move(cmd: &str) -> Option<(&str, &str)> {
    let rest = cmd.strip_prefix("move").or_else(|| cmd.strip_prefix('m'))?;
    let address =
        rest.starts_with(['+', '-', '$', '.']) || rest.starts_with(|c: char| c.is_ascii_digit());
    address.then(|| cmd.split_at(cmd.len() - rest.len()))
}

/// Splits a `:` line into a range, a command and its argument. `None` for a
/// blank line, which is not an error and not a command.
fn parse_ex(line: &str) -> Option<ExLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    // The lines the command applies to, off the front. A line that starts with
    // no address has no range and comes back whole, which is what lets every
    // command below read it exactly as it always did. See
    // `docs/specs/ranges.md`.
    let (scope, line) = crate::range::parse(line);
    if line.is_empty() {
        // A range and no command goes to its last line: `:42`, `:$`, `:%`.
        // Bare `'v` is the exception, and does nothing: it names where you
        // already are, and the prefill puts it there for you — so `:` then
        // Enter must not jump.
        return match scope {
            Some(Scope::Lines(range)) => Some(ExLine::Goto(range.last)),
            Some(Scope::Selection) | None => None,
        };
    }

    // `:g` and `:v` first, off the whole line: their pattern may contain
    // spaces and their command is a line of its own, so neither survives the
    // split-and-rejoin below.
    if let Some(parsed) = parse_global(line, scope) {
        return Some(parsed);
    }

    let (cmd, arg) = match line.split_once(char::is_whitespace) {
        Some((c, a)) => (c, a.trim()),
        None => (line, ""),
    };
    // Nothing after a space, so the argument may be stuck to the name.
    // `:s/a/b/ g` is not a thing anyone types, so the substitute split runs
    // whether or not a space was found — unlike `:m`'s, which only has to
    // catch a bare `:m+1`.
    let (cmd, arg) = match split_glued_substitute(cmd).or_else(|| split_glued_replace(cmd)) {
        Some((name, glued)) => (name, format!("{glued}{arg}")),
        None => match arg.is_empty() {
            true => {
                let (name, glued) = split_glued_move(cmd).unwrap_or((cmd, arg));
                (name, glued.to_string())
            }
            false => (cmd, arg.to_string()),
        },
    };
    let arg = arg.as_str();
    let force = cmd.ends_with('!');
    let name = cmd.trim_end_matches('!');
    let split = |dir| {
        ExLine::Window(WindowCmd::Split { dir, path: (!arg.is_empty()).then(|| arg.to_string()) })
    };

    // A range handed to a command that has no use for one is an error rather
    // than a range quietly dropped: vim writes part of a file for `:1,5w` and
    // bi does not, and a command that ignores half of what you typed is the
    // worse of the two ways to not support something.
    if scope.is_some()
        && !matches!(
            name,
            "m" | "move"
                | "s"
                | "substitute"
                | "ret"
                | "retab"
                | "case"
                | "sort"
                | "&"
                | "&&"
                | "d"
                | "yname"
                | "normal"
                | "norm"
        )
    {
        return Some(ExLine::Error(format!("`:{name}` takes no range")));
    }

    Some(match name {
        "w" | "write" => ExLine::Write(arg.into()),
        // The `a` forms mean "every buffer", which they genuinely do.
        "wa" | "wall" => ExLine::WriteAll,
        "qa" | "qall" => ExLine::QuitAll { force },
        "q" | "quit" => ExLine::Quit { force },
        "sp" | "split" => split(Dir::Horizontal),
        "vs" | "vsp" | "vsplit" => split(Dir::Vertical),
        // Not aliases for the two above: vim's `new` forms land on an empty
        // buffer. None takes a path, because `:new <path>` is `:sp <path>`.
        "new" => ExLine::Window(WindowCmd::New { dir: Some(Dir::Horizontal) }),
        "vnew" => ExLine::Window(WindowCmd::New { dir: Some(Dir::Vertical) }),
        "ene" | "enew" => ExLine::Window(WindowCmd::New { dir: None }),
        "clo" | "close" => ExLine::Window(WindowCmd::Close),
        "on" | "only" => ExLine::Window(WindowCmd::Only),
        // Bare `:e` reloads this buffer; with a path it changes what the
        // window shows, which is two different jobs under one name.
        "e" | "edit" if arg.is_empty() => ExLine::Revert { force },
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
        "themes" => ExLine::Themes,
        "yname" => match arg {
            "" => ExLine::Error("name it what? `:yname {register}`".into()),
            name => ExLine::Name { scope, name: name.into() },
        },
        // `on`/`off` as well as `true`/`false`: this one is spelled as a
        // command rather than as a setting, and a command reads better with the
        // words a switch uses.
        "ws" | "whitespace" => match arg {
            "" => ExLine::Whitespace(None),
            "on" | "true" => ExLine::Whitespace(Some(true)),
            "off" | "false" => ExLine::Whitespace(Some(false)),
            _ => ExLine::Error("whitespace takes on, off, or nothing to toggle".into()),
        },
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
        "m" | "move" => match parse_move(arg) {
            Some(to) => ExLine::Move { scope, to },
            None => ExLine::Error("move where? `:m +3`, `:m -2`, `:m 0`, `:m $`".into()),
        },
        "s" | "substitute" => match crate::substitute::parse(arg) {
            Ok(how) => ExLine::Substitute { scope, how },
            Err(message) => ExLine::Error(message),
        },
        // One meaning under both spellings — see `docs/specs/substitute.md`.
        // The argument is refused rather than dropped, the flags' own rule:
        // `:& g` quietly ignoring the `g` is how you believe it ran with it.
        "&" | "&&" => match arg.is_empty() {
            true => ExLine::SubstituteRepeat { scope },
            false => ExLine::Error(format!("nothing goes after `{name}` — got `{arg}`")),
        },
        // Short name only — `:delete` keeps meaning a path, and the argument
        // rule is what keeps the two apart. See `docs/specs/global.md`.
        "d" => match arg.is_empty() {
            true => ExLine::DeleteLines { scope },
            false => ExLine::Error(format!(
                "`:d` deletes lines and takes no argument — `:delete {arg}` deletes a path"
            )),
        },
        "normal" | "norm" => match arg.is_empty() {
            true => ExLine::Error("normal what? `:normal {keys}`".into()),
            false => ExLine::Normal { scope, keys: arg.to_string() },
        },
        "alt" | "alternate" => ExLine::Alternate,
        // Plural, because the command shows a list. `:sym` is the short form;
        // `:s` is taken by substitute and always will be.
        "sym" | "symbols" => ExLine::Symbols,
        "lsp" => match arg {
            "" => ExLine::Lsp(LspCmd::Status),
            "restart" => ExLine::Lsp(LspCmd::Restart),
            "stop" => ExLine::Lsp(LspCmd::Stop),
            _ => ExLine::Error("lsp takes restart, stop, or nothing for status".into()),
        },
        "def" | "definition" => ExLine::Definition,
        "decl" | "declaration" => ExLine::Declaration,
        "impl" | "implementation" => ExLine::Implementation,
        "peek" => ExLine::Peek,
        "ts" => ExLine::TsMarks,
        "tssplit" => ExLine::TsSplit,
        "tsjoin" => ExLine::TsJoin,
        "zen" => ExLine::Zen,
        // Plural like `:symbols`, and for the same reason.
        "diags" | "diagnostics" => ExLine::Diags,
        "refs" | "references" => ExLine::References,
        "fmt" | "format" if arg.is_empty() => ExLine::Format,
        "fmt" | "format" => {
            ExLine::Error("format takes no argument — it follows tab_width and expandtab".into())
        }
        "dn" | "dnext" => ExLine::DiagnosticJump { forward: true },
        "dp" | "dprev" => ExLine::DiagnosticJump { forward: false },
        "hover" => ExLine::Hover,
        // The whole argument is the pattern, spaces and all: `:find fn main`
        // has to search for `fn main`. That is also why there are no flags on
        // this line and a second command name instead — a `-r` would be a
        // pattern you could not search for.
        "find" | "find~" if !arg.is_empty() => {
            ExLine::Find { pattern: arg.into(), regex: name == "find~" }
        }
        "find" => ExLine::Error("find what?".into()),
        "find~" => ExLine::Error("find what pattern?".into()),
        // The argument is delimited, always — `/old/new/`, any delimiter, an
        // empty pattern meaning the pane's own search. The parse itself waits
        // for `run_replace`, which can see the filesystem a leading scope
        // needs.
        "replace" | "replace~" => ExLine::Replace { arg: arg.into(), regex: name == "replace~" },
        "results" => ExLine::ResultsPane,
        "res" | "resize" => match crate::resize::parse(arg) {
            Ok(how) => ExLine::Resize(how),
            Err(message) => ExLine::Error(message),
        },
        "sort" => match crate::sort::parse(arg, force) {
            Ok(how) => ExLine::Sort { scope, how },
            Err(message) => ExLine::Error(message),
        },
        "case" => match crate::case::Style::parse(arg) {
            Some(style) => ExLine::Case { scope, style },
            None => {
                ExLine::Error(format!("case what? one of {}", crate::case::Style::NAMES.join(", ")))
            }
        },
        // No argument: what it converts to is what the options already say,
        // and a `:retab 8` that disagreed with `tab_width` would be a second
        // place to set the same number.
        "ret" | "retab" if arg.is_empty() => ExLine::Retab(scope),
        "ret" | "retab" => {
            ExLine::Error("retab takes no argument — it follows tab_width and expandtab".into())
        }
        "wq" | "x" => ExLine::WriteQuit(arg.into()),
        "reload" => ExLine::ReloadConfig,
        // A bare line number never reaches here: it is a range with no
        // command, and was handled before the table.
        _ => ExLine::Unknown(name.into()),
    })
}

/// What `K` brought back, anchored where the question was asked — the cursor
/// may have moved on by the time the answer lands, and the float belongs to
/// the spot it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    pub window: WindowId,
    /// Char offset the float hangs from.
    pub anchor: usize,
    pub lines: Vec<HoverLine>,
    /// For highlighting the code lines — the buffer's filetype at request.
    pub language: Option<&'static str>,
}

/// One line of a hover, already sorted by what the frontend does with it:
/// code is highlighted through the grammar, a rule is drawn in the `rule`
/// style, text passes through. See `docs/specs/hover.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoverLine {
    Text(String),
    Code(String),
    Rule,
}

/// The parameters float: the active signature, the active parameter's span
/// in it, and how many more the server offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// Char offset the float hangs from — the cursor when the answer landed.
    pub anchor: usize,
    pub data: lsp::SignatureData,
}

/// A completion ask, parked between `apply` and `settle`. A request filed
/// during `apply` would reach the server *before* the `didChange` carrying
/// the char that triggered it — so `apply` marks, and `settle` sends after
/// the drain. See `docs/specs/complete.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompleteWant {
    /// Typing an identifier char asked.
    Word,
    /// A server trigger character (`.`, `::`) asked; the char rides along
    /// because the protocol wants servers told which one.
    Char(char),
    /// `Ctrl-N` asked, which is what earns failure a status line.
    Manual,
}

/// What `:lsp` was asked to do. Three values because the core has three
/// verbs: say where things stand, start over, stand down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspCmd {
    Status,
    Restart,
    Stop,
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
    /// `:new`, `:vnew`, `:enew` — an unnamed buffer, in a split or in this
    /// window. `None` is `:enew`: no split, this window.
    New {
        dir: Option<Dir>,
    },
    /// `Ctrl-W e` — a tree beside this window, rooted at the file it is
    /// showing, with that file selected.
    Tree,
    /// `Ctrl-W f` — a letter on every window, and the next key goes there.
    Pick,
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

/// What a key does in a window holding search results.
///
/// Four things: move, open, and leave. A results pane is a list you read and
/// take one row out of; it is not an editor, and every key it does not need is
/// a key that should keep meaning what it means everywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultsCmd {
    /// Rows, signed. `j`/`k`, and the arrows.
    Move(isize),
    First,
    Last,
    /// Enter — open the file at the line, replacing the results pane.
    Open,
    /// `Ctrl-^` back to what the pane was showing before the search.
    Close,
    /// `a` — apply the selected rewrite (or a heading's whole file), on an
    /// armed pane. See `docs/specs/find-in-files.md`.
    Apply,
    /// `A` — apply every rewrite still pending.
    ApplyAll,
    /// `x` — drop the row from the list: the hit, or a heading with
    /// everything under it. Edits nothing; narrows the offer.
    Remove,
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
    /// `gf` and `/` — the fuzzy list over this pane's rows.
    ///
    /// `whole` is the difference between the two keys: `gf` searches every
    /// path under the root and `/` searches only what is on screen. Both move
    /// the selection and open nothing. See `docs/specs/tree.md`.
    Find {
        whole: bool,
    },
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
    /// Counts opened images, so each carries a stable id a frontend can
    /// upload pixels under once. See `docs/specs/images.md`.
    next_image: u64,
    /// The area and chrome the frontend last laid out in.
    ///
    /// Splitting, resizing and directional switching are all geometry
    /// questions, and geometry needs a size. Before the first frame there is
    /// none, so those commands act on a zero area and say they had no room —
    /// which is true.
    area: Rect,
    chrome: Chrome,
    pub session: Session,
    config: Config,
    /// The palette named by `config.options.theme`, already resolved. Held
    /// rather than resolved per frame, because resolving reads a file.
    theme: Theme,
    /// Whether this session is editing from somewhere else — see
    /// [`Editor::set_remote`]. Selects `ssh_theme` over `theme`.
    remote: bool,
    /// Kept so `:reload` can ask again. `None` until a frontend supplies one —
    /// an embedder that wants no config never calls `load_config`, and
    /// `:reload` then has nothing to re-read and says so.
    config_source: Option<Box<dyn ConfigSource>>,
    config_epoch: u64,
    /// The language servers: running clients, their inbox, and the routing.
    /// See `docs/specs/lsp.md`.
    lsp: lsp::Registry,
    /// A completion ask parked until `settle` — see [`CompleteWant`].
    complete_want: Option<CompleteWant>,
    /// The newest completion request's number; only its answer is accepted.
    complete_seq: u64,
    /// A signature ask parked the same way; the char is the trigger when one
    /// opened it, `None` for the re-ask that follows the cursor.
    signature_want: Option<Option<char>>,
    signature_seq: u64,
    /// Whether `:normal` is running. It refuses to nest — replayed keys
    /// running the replayer is a loop with a keyboard in it. See
    /// `docs/specs/global.md`.
    replaying_normal: bool,
    /// How a buffer's git baseline is fetched — the index's copy of the file.
    /// `None` is an embedder or a test that wants no git, and every failure
    /// inside the loader is `None` too: there is nothing to say about a file
    /// git holds no copy of. See `docs/specs/git-signs.md`.
    git_baseline: Option<Box<dyn Fn(&Path) -> Option<String>>>,
}

/// One window and what it shows, borrowed to be drawn.
pub enum Pane<'a> {
    Text {
        window: &'a Window,
        text: &'a Text,
        buffer: &'a Buffer,
        syntax: Option<&'a Syntax>,
        /// This buffer's options, not the session's: a frontend draws a tab in
        /// a Makefile eight columns wide and the one in the file beside it
        /// four. See `docs/specs/options.md`.
        options: &'a Options,
    },
    Tree {
        window: &'a Window,
        tree: &'a Tree,
    },
    Results {
        window: &'a Window,
        results: &'a crate::results::Results,
    },
    Image {
        window: &'a Window,
        img: &'a crate::img::Img,
    },
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
    /// The options in force for this buffer — what every editing command here
    /// reads, in place of the session's. Mutable because a command can change
    /// the path under it, and a file that becomes a Makefile takes a
    /// Makefile's options with it.
    pub options: &'a mut Options,
    pub filetype: &'a mut Option<&'static str>,
    pub selections: &'a mut Selections,
    pub scroll: &'a mut usize,
    pub left: &'a mut usize,
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

/// Picks a grammar from the file's name. An unknown one yields `None`, which
/// renders as plain text.
///
/// The whole name rather than the extension, so a grammar can claim
/// `CMakeLists.txt`. Which key wins is `syntax.rs`'s business, not this
/// function's.
fn syntax_for(buffer: &Buffer, options: &Options) -> Option<Syntax> {
    wanted_syntax(buffer, options).and_then(|name| Syntax::for_filetype(name, buffer.rope()))
}

/// Which grammar this buffer should be read with, without parsing anything.
///
/// Split out so `resolve_options` can ask the question — every `:set` of every
/// option runs through there, and the answer decides whether a reparse is owed
/// at all.
///
/// `:set syntax` wins over the name, which is the whole of what it is for: a
/// file with no extension, or one whose extension lies.
fn wanted_syntax(buffer: &Buffer, options: &Options) -> Option<&'static str> {
    if let Some(named) = crate::syntax::canonical(&options.syntax) {
        return Some(named);
    }
    let name = buffer.path.as_ref()?.file_name()?.to_str()?;
    crate::syntax::filetype(name)
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
            let tree = Tree::new(path, editor.options().gitignore)?;
            // The directory bi was pointed at is the session's root from here
            // on, whatever displaces the tree later.
            editor.session.tree_root = Some(tree.root().to_path_buf());
            // Assigned rather than shown: there was nothing here before, so
            // there is no alternate to remember.
            editor.window_mut().content = Content::Tree(tree);
            return Ok(editor);
        }
        // An image opens as the image it is, exactly as a directory opens a
        // tree — and a failed decode falls through to the bytes as text,
        // which is what an image path always used to open as.
        if crate::img::looks_like_image(path)
            && let Ok(img) = crate::img::Img::open(path, 1)
        {
            let mut editor = Self::empty();
            editor.next_image = 2;
            editor.window_mut().content = Content::Image(img);
            return Ok(editor);
        }
        Ok(Self::with_buffer(Buffer::open(path)?))
    }

    /// Applies a config source, and remembers it for `:reload`.
    ///
    /// Called after construction rather than passed to [`Editor::open`] so
    /// that the three dozen existing call sites — nearly all tests — stay as
    /// they are, and so an embedder that wants no config simply never calls
    /// it.
    ///
    /// The returned diagnostics are the frontend's to show. Startup and
    /// `:reload` run this same path, which is the only way the two stay in
    /// agreement.
    /// Installs the world's clipboard, for `"+y` and `"+p`.
    ///
    /// Supplied by the frontend rather than built here, exactly as
    /// `load_config` supplies a `ConfigSource`: the library does not learn what
    /// a clipboard is. Without one, `"+` reports and changes nothing. See
    /// `docs/specs/clipboard.md`.
    pub fn set_clipboard(&mut self, clipboard: impl SystemClipboard + 'static) {
        self.session.system = Some(Box::new(clipboard));
    }

    pub fn load_config(&mut self, source: impl ConfigSource + 'static) -> Vec<Diagnostic> {
        let problems = self.read_config(&source).unwrap_or_else(|d| vec![d]);
        self.config_source = Some(Box::new(source));
        problems
    }

    /// The config as loaded — from `ConfigSource`, at startup or the most
    /// recent `:reload`.
    ///
    /// This is not the same thing as the options bi is running on: `:set`
    /// mutates `Session::options` alone, so after `:set number 5` this
    /// still reports whatever the file said. `apply_config` copies loaded
    /// options into `Session::options` on load, but the two copies diverge
    /// the moment `:set` touches one of them, so an option read through here
    /// can be stale. Read the *palette* through [`Editor::theme`] rather than
    /// this: `:set theme` moves `Session::options.theme` and re-resolves, and
    /// this copy keeps saying whatever the file said. Giving runtime state one
    /// owner is still open; see "Ownership" in `docs/specs/config.md`.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Reads and applies, without touching the stored source. Shared by
    /// [`Editor::load_config`] and [`Editor::reload_config`].
    ///
    /// `Err` is the unsalvageable case — the source could not be read, or
    /// the document is not TOML at all — and leaves the running config
    /// untouched. `Ok` carries the per-item problems, if any, from a
    /// document that did parse and was applied.
    ///
    /// Sets no status: startup and `:reload` have different things to say
    /// about the result, so that is left to the caller.
    fn read_config(&mut self, source: &dyn ConfigSource) -> Result<Vec<Diagnostic>, Diagnostic> {
        let mut problems = Vec::new();

        let mut config = match source.config() {
            Ok(Some(text)) => match crate::config::parse(&text, Config::default()) {
                Ok((config, main_problems)) => {
                    problems = main_problems;
                    config
                }
                // Unsalvageable: the running config stays exactly as it was.
                Err(problem) => return Err(problem),
            },
            // No file is not an error, but it is still a fact about the
            // world that can change between calls — a file present at
            // startup can be gone by `:reload`. Starting from the defaults
            // here, rather than from whatever was last applied, is what
            // keeps this path agreeing with startup on an editor that has
            // never seen a file at all.
            Ok(None) => Config::default(),
            Err(e) => return Err(Diagnostic { line: 1, message: e.to_string() }),
        };

        // The project's say, laid over the main config the way the main
        // config lay over the defaults — an option it does not mention keeps
        // what was already decided. The lookup was silent by contract; a
        // file that was *found* reports its mistakes like any config does,
        // each prefixed with the path so `:reload` names which file. One
        // that does not parse at all reports and changes nothing — the main
        // config stays whole, never half of each.
        // See `docs/specs/local-config.md`.
        if let Some((path, text)) = source.local() {
            let at = |line: usize, message: String| Diagnostic {
                line,
                message: format!("{}: {message}", path.display()),
            };
            match crate::config::parse_local(&text, config.clone()) {
                Ok((local, local_problems)) => {
                    config = local;
                    problems.extend(local_problems.into_iter().map(|p| at(p.line, p.message)));
                }
                Err(problem) => problems.push(at(problem.line, problem.message)),
            }
        }

        // Theme problems join config problems: both came out of loading, and
        // a frontend that reports one should report the other without
        // learning how many files there were.
        problems.extend(self.apply_config(config, Some(source)));
        Ok(problems)
    }

    fn apply_config(
        &mut self,
        config: Config,
        source: Option<&dyn ConfigSource>,
    ) -> Vec<Diagnostic> {
        self.session.options = config.options.clone();
        self.session.filetypes = config.filetypes.clone();
        // Whatever `:set` said this session is re-applied over the new file,
        // here and in every buffer below: a reload is a new config, not a new
        // session, and it must not undo what you typed.
        self.session.overrides.clone().apply_to(&mut self.session.options);
        self.config = config;
        self.config_epoch += 1;
        self.resolve_options();
        self.resolve_theme(source)
    }

    /// Recomputes every open buffer's options from the layers under them.
    ///
    /// Whole rather than incremental, and on every move of any layer — a
    /// config load, a `:set`, a buffer opening, a path changing. Working out
    /// which buffers a change could have reached costs more than redoing a
    /// handful of clones, and cannot be got wrong. See `docs/specs/options.md`.
    fn resolve_options(&mut self) {
        for entry in &mut self.buffers {
            let options =
                resolve_options(&self.session, entry.filetype, entry.buffer.path.as_deref());
            entry.options = options;
            // A `:set syntax` that left the parse tree alone would be a
            // setting that changed nothing on screen. Compared rather than
            // rebuilt every time: this runs on every `:set` of anything, and
            // reparsing every open buffer to find out that none of them
            // changed language is the wrong price for `:set number 3`.
            let wanted = wanted_syntax(&entry.buffer, &entry.options);
            if entry.syntax.as_ref().map(Syntax::filetype) != wanted {
                entry.syntax = syntax_for(&entry.buffer, &entry.options);
            }
        }
    }

    /// Turns the *name* in `options.theme` into a palette.
    ///
    /// Split from `apply_config` because `:set theme` reaches it too — the
    /// name can change without a config being loaded, and a theme the editor
    /// never re-resolved is a `:set` that silently did nothing.
    fn resolve_theme(&mut self, source: Option<&dyn ConfigSource>) -> Vec<Diagnostic> {
        let name = self.session.options.active_theme(self.remote).to_string();
        // A source that cannot be read is not fatal here: a missing themes/
        // directory is the normal case, and a built-in of that name is very
        // likely what was wanted anyway.
        let user = source.and_then(|s| s.theme(&name).ok().flatten());
        let (theme, problems) = Theme::resolve(&name, user.as_deref());
        self.theme = theme;
        self.apply_italics();
        problems
    }

    /// Keeps `self.theme` in step with `options.italics`.
    ///
    /// The palette is what the theme says; whether its italics survive is what
    /// the terminal can draw. Two questions, so the file keeps its
    /// `italic = true` and this decides whether it lands — in one place,
    /// called from the two that can move either half: a theme being resolved,
    /// and an editor being built with a theme it did not resolve.
    fn apply_italics(&mut self) {
        if !self.session.options.italics {
            self.theme.drop_italics();
        }
    }

    /// The resolved palette. A frontend maps [`crate::theme::Color`] to
    /// whatever it draws with; the core never names a terminal colour.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Says this session is editing from somewhere else, which swaps `theme`
    /// for `ssh_theme`.
    ///
    /// **The frontend decides, and the library is told.** Detecting a remote
    /// session means reading the environment — `SSH_CONNECTION` for the
    /// terminal — and the environment is process-wide, which makes it exactly
    /// the thing this codebase already refuses to reach for from a testable
    /// path: `main.rs` passes `$BI_CONFIG` and `$HOME` into `dir_from` for the
    /// same reason, so that two tests running at once cannot fight over them.
    /// An embedder that is a GUI, or a browser, has no `SSH_CONNECTION` to
    /// read and gets to answer the question itself.
    ///
    /// Order-independent: this re-resolves immediately if a config is already
    /// loaded, so a frontend may call it before or after `load_config`.
    pub fn set_remote(&mut self, remote: bool) {
        if self.remote == remote {
            return;
        }
        self.remote = remote;
        let source = self.config_source.take();
        self.resolve_theme(source.as_deref());
        self.config_source = source;
    }

    /// Whether `ssh_theme` is the one in force.
    pub fn is_remote(&self) -> bool {
        self.remote
    }

    /// Bumped every time a config is applied, so a frontend can tell that the
    /// keymap it installed is stale without diffing one.
    ///
    /// A counter rather than a callback: `Input` lives in the frontend and
    /// `:reload` happens in here, so the two need *some* way to meet, and a
    /// number the frontend reads when it likes is the one that adds no
    /// ownership between them.
    pub fn config_epoch(&self) -> u64 {
        self.config_epoch
    }

    /// Re-reads the config through the source a frontend supplied, and swaps
    /// every option at once.
    ///
    /// A failed reload changes nothing. Reloading yourself into an unusable
    /// config, with no way to type `:reload` again, is the one outcome worth
    /// engineering against — so a document that does not parse is reported
    /// and discarded, and the running config stays.
    fn reload_config(&mut self) {
        // Taken and put back: `read_config` needs `&mut self`, and the
        // source lives on `self`.
        let Some(source) = self.config_source.take() else {
            self.session.status = "no config to reload".into();
            return;
        };

        let result = self.read_config(source.as_ref());
        self.config_source = Some(source);

        self.session.status = match result {
            Ok(problems) => match problems.len() {
                0 => "config reloaded".into(),
                n => format!("config reloaded — {n} problem{}", if n == 1 { "" } else { "s" }),
            },
            Err(problem) => {
                format!("config not reloaded — line {}: {}", problem.line, problem.message)
            }
        };
    }

    fn with_buffer(buffer: Buffer) -> Self {
        let (buffer_id, window_id) = (BufferId(0), WindowId(0));
        let mut editor = Self {
            buffers: vec![BufferEntry::new(buffer_id, buffer)],
            windows: vec![Window::new(window_id, buffer_id)],
            layout: Layout::new(window_id),
            focus: window_id,
            previous: None,
            next_buffer: 1,
            next_window: 1,
            next_image: 1,
            area: Rect::default(),
            chrome: Chrome::default(),
            session: Session::default(),
            config: Config::default(),
            theme: Theme::default(),
            remote: false,
            config_source: None,
            config_epoch: 0,
            lsp: lsp::Registry::default(),
            complete_want: None,
            complete_seq: 0,
            signature_want: None,
            signature_seq: 0,
            replaying_normal: false,
            git_baseline: None,
        };
        editor.resolve_options();
        editor.apply_italics();
        editor
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
            Content::Results(results) => Pane::Results { window, results },
            Content::Image(img) => Pane::Image { window, img },
            Content::Text(text) => {
                let entry = self.entry(text.buffer);
                Pane::Text {
                    window,
                    text,
                    buffer: &entry.buffer,
                    syntax: entry.syntax.as_ref(),
                    options: &entry.options,
                }
            }
        })
    }

    /// The options in force where the cursor is.
    ///
    /// The focused buffer's, not the session's — an embedder asking how wide a
    /// tab is means *here*, and the answer differs per file. A tree pane shows
    /// no buffer and gets the session's, which is the only honest answer for a
    /// pane that is not a file.
    pub fn options(&self) -> &Options {
        match self.window_of(self.focus).and_then(Window::buffer) {
            Some(id) => &self.entry(id).options,
            None => &self.session.options,
        }
    }

    /// One buffer's options, for a frontend that is drawing a window it is not
    /// focused on.
    pub fn options_of(&self, id: BufferId) -> &Options {
        &self.entry(id).options
    }

    /// `Ctrl-W f` — puts a letter on every window and waits for one.
    ///
    /// Every window, the focused one included: jumping to where you already
    /// are is a no-op, and leaving it out would move the letters around
    /// depending on where you were, which is what a label is supposed not to
    /// do. One window is not worth a mode, so it says so and stays put.
    fn pick_window(&mut self) {
        let ids: Vec<WindowId> = self.windows.iter().map(|w| w.id).collect();
        if ids.len() < 2 {
            self.session.status = "only one window".into();
            return;
        }
        let targets = crate::label::labels(ids.len(), &[])
            .into_iter()
            .zip(ids)
            .map(|(label, id)| (label, LabelTarget::Window(id)))
            .collect();
        self.session.labels = Some(Labels { typed: String::new(), targets });
        self.session.mode = Mode::Label;
    }

    /// `S` — a letter at both ends of every scope around the cursor.
    ///
    /// The chain of tree-sitter nodes containing the cursor, innermost first,
    /// lettered `a`, `b`, `c` — the alphabet rather than the home row, because
    /// here the letters mean an *order* and `a` inside `b` inside `c` says so
    /// at a glance. See `docs/specs/scopes.md`.
    fn show_scopes(&mut self) {
        let Some(view) = self.focused() else { return };
        let at = view.selections.cursor().at;
        let Some(syntax) = view.syntax.as_ref() else {
            self.session.status = "no parse tree for this file".into();
            return;
        };
        let rope = view.buffer.rope();
        let byte = rope.char_to_byte(at.min(rope.len_chars()));
        let scopes: Vec<(usize, usize)> = syntax
            .scopes_at(byte)
            .into_iter()
            .map(|range| (rope.byte_to_char(range.start), rope.byte_to_char(range.end)))
            .filter(|(start, end)| end > start)
            .collect();

        if scopes.is_empty() {
            self.session.status = "nothing around the cursor".into();
            return;
        }
        let targets = crate::label::labels_from(scopes.len(), crate::label::ALPHABET, &[])
            .into_iter()
            .zip(scopes)
            .map(|(label, (start, end))| (label, LabelTarget::Scope(start, end)))
            .collect();
        self.session.labels = Some(Labels { typed: String::new(), targets });
        self.session.mode = Mode::Label;
    }

    /// `s` — dims the screen and waits for something to look for.
    fn enter_find(&mut self) {
        self.session.find = Some(Find { query: String::new(), matches: Vec::new() });
        self.session.labels = None;
        self.session.mode = Mode::Find;
    }

    fn end_find(&mut self) {
        self.session.find = None;
        self.session.labels = None;
        self.session.mode = Mode::Normal;
    }

    /// A key pressed while `s` is aiming.
    ///
    /// A label wins over a query character, and it can: the labels are chosen
    /// so that no character which could extend a match is ever one. That one
    /// rule is what lets typing and jumping share a keyboard with no mode
    /// switch between them.
    fn find_key(&mut self, c: char) {
        let pending = self.session.labels.as_ref().is_some_and(|labels| {
            !labels.typed.is_empty() || labels.targets.iter().any(|(label, _)| label.starts_with(c))
        });
        if pending {
            return self.label_key(Some(c));
        }
        if let Some(find) = &mut self.session.find {
            find.query.push(c);
        }
        self.refresh_find();
    }

    /// Backspace: take a character back, and leave when there is none.
    fn find_backspace(&mut self) {
        let Some(find) = &mut self.session.find else { return self.end_find() };
        if find.query.pop().is_none() {
            return self.end_find();
        }
        self.refresh_find();
    }

    /// Finds the query in the focused window's viewport and labels what it
    /// found.
    ///
    /// The viewport and nothing else: this is a jump, not a search. Something
    /// you cannot see is not somewhere you are aiming, and a letter on it
    /// could not be drawn anyway.
    fn refresh_find(&mut self) {
        let Some(find) = &self.session.find else { return };
        let query = find.query.clone();
        if query.is_empty() {
            if let Some(find) = &mut self.session.find {
                find.matches.clear();
            }
            self.session.labels = None;
            return;
        }

        let Some((from, to)) = self.visible_range() else { return self.end_find() };
        let Some(buffer) = self.buffer() else { return self.end_find() };
        let matches = buffer.matches_in(from, to, &query, false);
        if matches.is_empty() {
            // Nothing to press and nothing to narrow: the next key was going
            // to be `Esc` either way.
            return self.end_find();
        }

        // Every character that could extend *any* match on screen, in both
        // cases — so which match you were looking at cannot change the answer.
        let mut exclude: Vec<char> = Vec::new();
        for &(_, end) in &matches {
            let Some(next) = buffer.char_at(end) else { continue };
            for c in next.to_lowercase().chain(next.to_uppercase()) {
                if !exclude.contains(&c) {
                    exclude.push(c);
                }
            }
        }

        // What you have typed, where every other half-typed command shows
        // itself.
        self.session.status = format!("s {query}");
        let targets = crate::label::labels(matches.len(), &exclude)
            .into_iter()
            .zip(&matches)
            .map(|(label, &(start, _))| (label, LabelTarget::Position(start)))
            .collect();
        self.session.labels = Some(Labels { typed: String::new(), targets });
        if let Some(find) = &mut self.session.find {
            find.matches = matches;
        }
    }

    /// The char range the focused window is showing.
    fn visible_range(&self) -> Option<(usize, usize)> {
        let window = self.window_of(self.focus)?;
        let text = window.text()?;
        let buffer = self.buffer()?;
        let rope = buffer.rope();
        let last = (text.scroll + window.height).min(buffer.line_count());
        let from = rope.line_to_char(text.scroll.min(rope.len_lines().saturating_sub(1)));
        let to = match last >= buffer.line_count() {
            true => rope.len_chars(),
            false => rope.line_to_char(last),
        };
        Some((from, to))
    }

    /// A key pressed while the letters are up.
    ///
    /// A key that cannot start any label cancels rather than being swallowed,
    /// which is what makes a mistyped label cost one press instead of two.
    fn label_key(&mut self, c: Option<char>) {
        let Some(labels) = &mut self.session.labels else {
            self.session.mode = Mode::Normal;
            return;
        };
        let Some(c) = c else { return self.end_labels() };
        labels.typed.push(c);
        match labels.resolve() {
            Resolution::Pending => {}
            Resolution::Miss => self.end_labels(),
            Resolution::Hit(target) => {
                self.end_labels();
                match target {
                    LabelTarget::Window(id) => self.set_focus(id),
                    // The start of the match, because you aimed at the word
                    // and the letter was drawn after it.
                    LabelTarget::Position(at) => self.set_cursor(Cursor::at(at)),
                    LabelTarget::Scope(start, end) => {
                        if let Some(view) = self.focused() {
                            // Charwise visual is inclusive of the head, so the
                            // head sits *on* the last character rather than
                            // past it — the same rule `viw` follows.
                            view.selections.set(vec![Selection {
                                anchor: Cursor::at(start),
                                head: Cursor::at(end.saturating_sub(1).max(start)),
                            }]);
                        }
                        self.session.mode = Mode::Visual(Shape::Chars);
                    }
                }
            }
        }
    }

    /// Puts the letters away, and whatever was aiming with them: a jump that
    /// has landed has no query left to narrow.
    fn end_labels(&mut self) {
        self.session.labels = None;
        self.session.find = None;
        self.session.mode = Mode::Normal;
    }

    /// Everything to draw over `rows` of `window` that is not buffer text, in
    /// paint order.
    ///
    /// One call per pane per frame, bounded by the rows on screen and never by
    /// the size of the file — the same rule the highlight pass follows.
    /// Nothing is cached: a decoration is derived from the buffer, the options
    /// and the theme, and a cache over those would need invalidating on every
    /// edit, every `:set` and every scroll, which is more work than the
    /// derivation it would avoid. See `docs/specs/decorations.md`.
    pub fn decorations(
        &self,
        window: WindowId,
        rows: std::ops::Range<usize>,
    ) -> Vec<crate::decoration::Decoration> {
        let mut out = Vec::new();
        // Before the text pane below, because a window wearing a letter may be
        // showing a tree, which has no buffer for the rest of this to read.
        if let Some(labels) = &self.session.labels
            && window == self.focus
            && let Some(Pane::Text { buffer, options, .. }) = self.pane(window)
        {
            // The closing letters innermost first, the opening ones outermost
            // first, so the whole list nests the way brackets do —
            // `c{ b"ahello/plugina"b }c` — and two scopes sharing an edge read
            // as `ab` rather than as one letter hiding the other. The renderer
            // keeps the order they are pushed in where the column is the same.
            let scope = |at: usize, on: usize, label: &str, out: &mut Vec<_>| {
                let row = buffer.row_at(Cursor::at(on));
                if !rows.contains(&row) {
                    return;
                }
                let line = buffer.line(row);
                let col = crate::indent::display_col(
                    &line,
                    at - buffer.rope().line_to_char(row),
                    options.tab_width,
                );
                out.push(crate::decoration::Decoration::Inline {
                    row,
                    col,
                    text: label.to_string(),
                    style: self.theme.ui.label,
                });
            };
            for (label, target) in &labels.targets {
                let LabelTarget::Scope(_, end) = target else { continue };
                if !label.starts_with(&labels.typed) {
                    continue;
                }
                // In front of the cell *after* the last character, which is
                // where a closing letter belongs and is why the row is taken
                // from the character itself.
                scope(*end, end.saturating_sub(1), label, &mut out);
            }
            for (label, target) in labels.targets.iter().rev() {
                let LabelTarget::Scope(start, _) = target else { continue };
                if !label.starts_with(&labels.typed) {
                    continue;
                }
                scope(*start, *start, label, &mut out);
            }
        }
        if let Some(labels) = &self.session.labels {
            for (label, target) in &labels.targets {
                let LabelTarget::Window(id) = target else { continue };
                if *id != window || !label.starts_with(&labels.typed) {
                    continue;
                }
                out.extend(self.window_label(window, label, &rows));
            }
        }
        let Some(Pane::Text { buffer, options, syntax, text, window: pane }) = self.pane(window)
        else {
            return out;
        };
        // `s` is aiming: everything dims, what matched lights up, and each
        // match wears the letter that goes to it. Only in the window it is
        // aiming in. The order these are pushed in is the order they paint in,
        // so the dim goes down first and the matches over it.
        //
        // From the press, not from the first letter typed: the dim *is* the
        // announcement that `s` is aiming, and one that arrives a keystroke
        // later leaves you looking at a normal screen wondering whether the
        // key registered.
        if window == self.focus
            && let Some(find) = &self.session.find
        {
            find_decorations(
                buffer,
                find,
                self.session.labels.as_ref(),
                &self.theme,
                options,
                &rows,
                &mut out,
            );
        }
        // `:ts` — every boundary on show. The same dim `s` uses, and for the
        // same reason: the text is background while the marks are the
        // subject. Focused window only, recomputed each frame from the live
        // tree, so it cannot go stale. See `docs/specs/boundaries.md`.
        if self.session.ts_marks
            && window == self.focus
            && let Some(syntax) = syntax
        {
            boundary_marks(buffer, syntax, &self.theme, &rows, &mut out);
        }
        // The guides stand down while the blanks are on show. On a line with
        // text a bullet would win the column anyway, but a guide at column 0
        // of an *empty* line has no character under it to be won by, and one
        // inside a wide tab has none either — so it survives, and reads as a
        // space that is not there. That is the exact opposite of what this
        // mode is for. The two answer different questions, and nobody needs
        // both answers at once: guides are for reading structure, this is for
        // auditing what the file actually contains.
        if options.indent_guides && !options.whitespace {
            indent_guides(buffer, options, &self.theme, rows.clone(), &mut out);
        }
        // Before the context marks, so the pilcrow sits at the true end of the
        // line and the `} // if ...` that follows it reads as being past the
        // line rather than inside it.
        if options.whitespace {
            whitespace(buffer, options, &self.theme, rows.clone(), &mut out);
        }
        if options.todo_comments {
            todo_comments(buffer, &self.theme, rows.clone(), &mut out);
        }
        // What the server said is wrong, worn by the text it names — under
        // the selection, like a TODO tag. A zero-width diagnostic ("missing
        // semicolon *here*") still gets one cell to wear.
        if options.diagnostics
            && let Some(id) = self.window_of(window).and_then(Window::buffer)
        {
            let len = buffer.rope().len_chars();
            for d in self.diagnostics(id) {
                if buffer.row_at(Cursor::at(d.start)) >= rows.end
                    || buffer.row_at(Cursor::at(d.end)) < rows.start
                {
                    continue;
                }
                let end = d.end.max(d.start + 1).min(len);
                if d.start >= end {
                    continue;
                }
                out.push(crate::decoration::Decoration::Repaint {
                    range: d.start..end,
                    style: self.diag_style(d.severity),
                    layer: crate::decoration::Layer::Under,
                });
            }
            // The message rides the cursor's row — the row where "what is
            // wrong here" is being asked — and only in the focused window,
            // for the same reason the context marks stay there.
            if window == self.focus {
                let cursor_row = buffer.row_at(text.selections.cursor());
                let on_row: Vec<&lsp::Diag> = self
                    .diagnostics(id)
                    .iter()
                    .filter(|d| buffer.row_at(Cursor::at(d.start)) == cursor_row)
                    .collect();
                if rows.contains(&cursor_row)
                    && let Some(first) = on_row.first()
                {
                    let mut message = first.message.lines().next().unwrap_or("").to_string();
                    if on_row.len() > 1 {
                        message.push_str(&format!(" (+{})", on_row.len() - 1));
                    }
                    // The underline comes off: it marks a *range*, and this
                    // text is bi's annotation past the end of one.
                    let mut style = self.diag_style(first.severity);
                    style.underline = false;
                    out.push(crate::decoration::Decoration::Eol {
                        row: cursor_row,
                        text: format!("  ■ {message}"),
                        style,
                    });
                }
            }
        }
        // Only in the focused window: an unfocused pane's cursor is not where
        // you are looking, and annotating a brace for reasons off the screen
        // is worse than not annotating it.
        if window == self.focus
            && let Some(syntax) = syntax
        {
            context_marks(buffer, syntax, text, options, &self.theme, rows.clone(), &mut out);
            context_header(
                buffer,
                syntax,
                text,
                pane.width,
                options,
                &self.theme,
                rows.clone(),
                &mut out,
            );
        }
        if options.color_swatches {
            color_swatches(buffer, rows, &mut out);
        }
        if let Some(flash) = &self.session.flash
            && self.window_of(window).and_then(Window::buffer) == Some(flash.buffer)
            && std::time::Instant::now() < flash.until
        {
            for range in &flash.ranges {
                out.push(crate::decoration::Decoration::Repaint {
                    range: range.clone(),
                    style: self.theme.ui.flash,
                    layer: crate::decoration::Layer::Under,
                });
            }
        }
        out
    }

    /// One window's letter for `Ctrl-W f`, as a block in the middle of it.
    ///
    /// Three rows tall and two cells wider than the letter, painted in the
    /// theme's `label` with the letter in the centre. A single character in a
    /// corner is what this was, and on a screen of four panes of code it is
    /// one more character on a screen full of them — you have to hunt for the
    /// thing that exists to save you hunting.
    ///
    /// **Over the text, not inserted into it**, which is the opposite of what
    /// every other label does (`docs/specs/labels.md`). The rule there is that
    /// a label must not hide the character it points at; this one points at a
    /// *window*, and the nine cells it covers are nine cells it is not talking
    /// about. Nothing is lost that the next keystroke does not give straight
    /// back.
    ///
    /// Centred on the middle of what the pane is showing. A row of the box
    /// that falls outside it is simply not drawn — a two-line file gets less
    /// of a box, which still reads, rather than a box drawn over the `~`s
    /// where there is no line to decorate.
    fn window_label(
        &self,
        window: WindowId,
        label: &str,
        rows: &std::ops::Range<usize>,
    ) -> Vec<crate::decoration::Decoration> {
        if rows.is_empty() {
            return Vec::new();
        }
        let width = label.chars().count() + 2;
        // The gutter is the frontend's to draw and the core's to know: a
        // column of the *text area* is what a decoration names, so the middle
        // of the pane is the middle of what is left after the numbers.
        let gutter = match self.pane(window) {
            Some(Pane::Text { buffer, options, .. }) => options.gutter_width(buffer.line_count()),
            _ => 0,
        };
        let text_width = self.window_of(window).map_or(0, |w| w.width).saturating_sub(gutter);
        let col = text_width.saturating_sub(width) / 2;

        let middle = rows.start + (rows.end - rows.start) / 2;
        (middle.saturating_sub(1)..=middle + 1)
            .filter(|row| rows.contains(row))
            .map(|row| crate::decoration::Decoration::Overlay {
                row,
                col,
                text: match row == middle {
                    true => format!(" {label} "),
                    false => " ".repeat(width),
                },
                style: self.theme.ui.label,
                layer: crate::decoration::Layer::Over,
            })
            .collect()
    }

    /// How long until something on screen changes on its own, if anything
    /// will.
    ///
    /// The whole of what the yank flash asks of a frontend: a loop that
    /// blocked on the next key now waits for the next key *or* for this,
    /// whichever comes first, and draws either way. Clears a flash that has
    /// already expired, which is what stops "expired zero seconds ago" from
    /// being a timeout to spin on.
    pub fn redraw_in(&mut self) -> Option<std::time::Duration> {
        let flash = self.session.flash.as_ref()?;
        let left = flash.until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            self.session.flash = None;
            return None;
        }
        Some(left)
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
        let span =
            crate::region::block_span_at(buffer, &text.selections, self.session.block_to_eol, row);
        (span.start, span.end)
    }

    /// What kind of selection is on screen, if any.
    ///
    /// Not `Mode::visual()`, because two modes can have one: the `:` line
    /// inherits the selection it interrupted, so that a rectangle is still a
    /// rectangle while you type the command that is about to act on it. Every
    /// other mode answers `None` whatever [`Session::visual`] happens to hold,
    /// which is what keeps a stale value from painting.
    pub fn visual(&self) -> Option<Shape> {
        self.session.visual()
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
            options: &mut entry.options,
            filetype: &mut entry.filetype,
            selections: &mut text.selections,
            scroll: &mut text.scroll,
            left: &mut text.left,
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
        self.buffers.push(BufferEntry::new(id, buffer));
        self.refresh_git(id);
        self.resolve_options();
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
        self.touch(to);
        let Some(current) = self.window_of(window) else { return };

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
        // A results pane about to be displaced is kept for `:results`.
        self.park_results(window);
        let window = self.window_mut_of(window).expect("checked above");
        window.show(Content::Text(text));
        self.sweep_scratch();
    }

    /// Drops a `[No Name]` nobody is looking at.
    ///
    /// Unnamed, empty and unmodified, and displayed by no window: there is
    /// nothing in it, nothing to save it to, and no way back to it. Keeping it
    /// only puts a blank in the `Ctrl-I` cycle.
    ///
    /// This is what stops `bi .` leaving one behind forever. `Editor::open` on
    /// a directory makes an empty session and then replaces the window's
    /// content with the tree, orphaning the buffer it just made; the sweep
    /// collects it as soon as a real file arrives to keep the list non-empty.
    /// Plain `bi` followed by `:e file` lands in the same place, which is how
    /// vim reuses its initial blank.
    ///
    /// Each condition earns its place. Unnamed, or `:w` would have somewhere
    /// to write. Unmodified *and* empty, because a buffer emptied by deleting
    /// its text is modified and its undo history is a reason to keep it. Shown
    /// nowhere, which is what makes it unreachable — a `:enew` you are looking
    /// at survives.
    fn sweep_scratch(&mut self) {
        if self.buffers.len() < 2 {
            return;
        }
        let doomed: Vec<BufferId> = self
            .buffers
            .iter()
            .filter(|entry| {
                entry.buffer.path.is_none()
                    && entry.buffer.rope().len_chars() == 0
                    && !entry.buffer.is_modified()
            })
            .map(|entry| entry.id)
            .filter(|id| !self.windows.iter().any(|w| w.buffer() == Some(*id)))
            .collect();

        for id in doomed {
            // The invariant outranks the sweep: something has to be left.
            if self.buffers.len() < 2 {
                return;
            }
            // An alternate pointing at a blank is not worth a `Ctrl-^`, and a
            // `BufferId` resolving to nothing is the one way a stable id is
            // worse than an index.
            for window in &mut self.windows {
                if window.alt_buffer() == Some(id) {
                    window.alt = None;
                }
            }
            self.buffers.retain(|entry| entry.id != id);
        }
    }

    fn run_buffer_cmd(&mut self, cmd: BufferCmd) {
        let focus = self.focus;

        // A parked tree is an alternate like any other, and swapping back is
        // the whole reason displacing it is tolerable: re-reading the
        // directory would lose the expansion you had built up.
        // Checked before taking: a buffer alternate must stay where it is, and
        // `take` on a failed pattern would still have emptied the slot.
        if cmd == BufferCmd::Alternate
            && matches!(
                self.window().alt,
                Some(Content::Tree(_) | Content::Results(_) | Content::Image(_))
            )
        {
            let parked = self.window_mut().alt.take().expect("checked above");
            return self.window_mut().show(parked);
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
            // Into the most-recent-first list, which is the order the picker
            // built its rows from and cannot change while it holds every key.
            BufferCmd::Chosen(i) => self.mru_ids().get(i).copied(),
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
                    // An image has no buffer to delete, but `:bd` still means
                    // "take this away", and refusing it on a technicality is
                    // exactly the detached feeling docs/specs/images.md exists
                    // to avoid. Discarded rather than parked — that is what
                    // delete means; `Ctrl-^` is the key that parks.
                    None if self.window().img().is_some() => self.dismiss_image(focus),
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

    /// Removes a buffer from the list, and with it the windows that showed it.
    ///
    /// Three splits on one file are three views of one thing; deleting it and
    /// falling through would leave three views of some other file nobody asked
    /// to see three times. The layout collapses around the closed windows
    /// through [`Layout::close`], which is the same collapse `Ctrl-W c` gets —
    /// there is one close, and this is not a second.
    ///
    /// The focused window survives when every window showed the buffer: one has
    /// to, since the last window cannot close, and leaving it where the user is
    /// already looking means focus never moves.
    fn delete_buffer(&mut self, id: BufferId, force: bool) {
        if self.is_modified(id) && !force {
            self.session.status = "unsaved changes (use `:bd!` to discard)".into();
            return;
        }

        // The server hears first: a `didClose` after the list forgot the
        // entry would have nothing left to say.
        if let Some(entry) = self.buffers.iter().find(|b| b.id == id)
            && let lsp::Attach::Doc(doc) = &entry.lsp
        {
            let doc = doc.clone();
            self.lsp.close(&doc);
        }

        let ids = self.buffer_ids();
        let at = ids.iter().position(|&b| b == id).unwrap_or(0);
        let name = self.name_of(id);

        // The focused window goes last, so that when it is the only one left to
        // close it is the one `Layout::close` refuses — and the pane the user
        // is looking at is the pane that stays. At most one refuses, since only
        // the last window in a session can.
        let mut showing: Vec<WindowId> =
            self.windows.iter().filter(|w| w.buffer() == Some(id)).map(|w| w.id).collect();
        showing.sort_by_key(|&w| w == self.focus);
        let orphan = showing
            .into_iter()
            .fold(None, |orphan, w| if self.close_window(w) { orphan } else { Some(w) });

        // Only now, because closing a window sweeps blanks nobody is looking
        // at, and a fresh heir is exactly that until it is shown.
        if let Some(w) = orphan {
            let heir =
                if ids.len() == 1 { self.fresh_scratch() } else { ids[(at + 1) % ids.len()] };
            self.show(w, heir);
        }

        // A stable id that resolves to nothing is the one way it is worse than
        // an index, which at least fails loudly.
        for window in &mut self.windows {
            if window.alt_buffer() == Some(id) {
                window.alt = None;
            }
        }

        self.buffers.retain(|b| b.id != id);
        // The list is never empty, so no path has to handle a session with
        // nothing open — and closing the last window that showed the last
        // buffer is the other way it could have become so. (A window may still
        // show no buffer; that is a tree, not an empty list.)
        if self.buffers.is_empty() {
            self.fresh_scratch();
        }
        self.session.status = format!("\"{name}\" deleted");
    }

    /// `:alt` — opens the other file, if one of them is there.
    ///
    /// The first rule whose pattern matches decides, and then the first of its
    /// paths that exists is opened. A path that does not exist is not offered
    /// and not created: `:alt` finds the other file, and making one is `:e`'s
    /// job — with the name it just told you. See `docs/specs/alternate.md`.
    fn open_alternate(&mut self) {
        let Some(path) = self.buffer().and_then(|b| b.path.clone()) else {
            self.session.status = "no file name".into();
            return;
        };
        let name = path.to_string_lossy().to_string();
        let candidates = crate::alternate::candidates(&self.config.alternates, &name);
        if candidates.is_empty() {
            self.session.status = format!("no alternate for {name}");
            return;
        }
        match candidates.iter().find(|candidate| std::path::Path::new(candidate).exists()) {
            Some(found) => {
                let found = found.clone();
                self.edit_path(&found)
            }
            None => self.session.status = format!("none of {} is there", candidates.join(", ")),
        }
    }

    /// Moves `id` to the front of the most-recently-shown list.
    ///
    /// Shown rather than focused: a buffer you put in a split beside you is
    /// one you are working with, and the list is about what you are working
    /// with rather than about where the cursor happens to be.
    fn touch(&mut self, id: BufferId) {
        self.session.mru.retain(|&seen| seen != id);
        self.session.mru.insert(0, id);
    }

    /// Every open buffer, most recently shown first.
    ///
    /// Anything the list has not heard of goes on the end in the order it was
    /// opened, so a buffer that arrived without being shown — `:badd` has no
    /// spelling here yet, but `:wa` walks them all — is still reachable.
    fn mru_ids(&self) -> Vec<BufferId> {
        let open = self.buffer_ids();
        let mut out: Vec<BufferId> =
            self.session.mru.iter().copied().filter(|id| open.contains(id)).collect();
        let missing: Vec<BufferId> = open.into_iter().filter(|id| !out.contains(id)).collect();
        out.extend(missing);
        out
    }

    /// `Ctrl-P` — the picker over every file under the session's root.
    /// `:find <pattern>` — every match under the project root, in a pane.
    ///
    /// The pane replaces what the focused window was showing, keeping it as the
    /// alternate, so `Ctrl-^` puts your file back. A split first is how you get
    /// both at once, which is one keystroke and saves this command a policy
    /// about where results ought to live.
    /// Keeps a results pane that is about to leave `id`'s content slot, so
    /// `:results` can bring it back — selection, prunes and applied marks
    /// intact. Cheap when the content is anything else.
    fn park_results(&mut self, id: WindowId) {
        let parked = match self.window_of(id).map(|w| &w.content) {
            Some(Content::Results(results)) => Some(results.clone()),
            _ => None,
        };
        if let Some(results) = parked {
            self.session.parked_results = Some(results);
        }
    }

    /// `:results` — the last list back, as it was.
    fn reopen_results(&mut self) {
        if self.window().results().is_some() {
            self.session.status = "already looking at them".into();
            return;
        }
        let Some(results) = self.session.parked_results.take() else {
            self.session.status = "no results to bring back — `:find` something first".into();
            return;
        };
        let focus = self.focus;
        if let Some(window) = self.window_mut_of(focus) {
            window.show(Content::Results(results));
        }
    }

    /// Shows `results` in the focused window, parking whatever list it
    /// displaces.
    fn show_results(&mut self, results: crate::results::Results) {
        let focus = self.focus;
        self.park_results(focus);
        if let Some(window) = self.window_mut_of(focus) {
            window.show(Content::Results(Box::new(results)));
        }
    }

    /// Splits a leading directory scope off a `:find`/`:replace` argument.
    ///
    /// A first word ending in `/` that names a directory under `root` is a
    /// scope. Both conditions, deliberately: the trailing slash is you saying
    /// "directory", and the existence check is what keeps a pattern like
    /// `foo/ bar` searchable — a word naming no directory stays in the
    /// pattern, and the status line echoes what was searched, so a typo'd
    /// scope reads back as the literal it became. Absolute paths are not
    /// scopes: the project root is the promise `:find` makes.
    fn split_scope<'a>(root: &Path, arg: &'a str) -> (Option<PathBuf>, &'a str) {
        let Some((first, rest)) = arg.split_once(char::is_whitespace) else {
            return (None, arg);
        };
        let scoped = first.len() > 1
            && first.ends_with('/')
            && !first.starts_with('/')
            && root.join(first).is_dir();
        match scoped {
            true => (Some(PathBuf::from(first)), rest.trim_start()),
            false => (None, arg),
        }
    }

    /// Walks the project — or `scope` under it — for `query`. Matches come
    /// back with the scope joined onto their paths, so a row reads the same
    /// wherever the walk started and Enter opens the right file.
    fn search_project(
        &self,
        root: &Path,
        scope: Option<&Path>,
        query: &crate::find_in_files::Query,
    ) -> Result<crate::find_in_files::Found, String> {
        let walk = match scope {
            Some(scope) => root.join(scope),
            None => root.to_path_buf(),
        };
        let mut found = crate::find_in_files::search(&walk, query)?;
        if let Some(scope) = scope {
            for m in &mut found.matches {
                m.path = scope.join(&m.path);
            }
        }
        Ok(found)
    }

    /// How many files a match list spans — they arrive grouped by file.
    fn files_of(matches: &[crate::find_in_files::Match]) -> usize {
        let mut n = 0;
        let mut last: Option<&std::path::PathBuf> = None;
        for m in matches {
            if last != Some(&m.path) {
                n += 1;
                last = Some(&m.path);
            }
        }
        n
    }

    fn run_find(&mut self, arg: &str, regex: bool) {
        let root = self.tree_root(self.buffer().and_then(|b| b.path.as_deref()));
        let (scope, pattern) = Self::split_scope(&root, arg);
        let query = crate::find_in_files::Query {
            pattern: pattern.to_string(),
            regex,
            gitignore: self.options().gitignore,
            ..Default::default()
        };

        let found = match self.search_project(&root, scope.as_deref(), &query) {
            Ok(found) => found,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };

        if found.matches.is_empty() {
            // Nothing found leaves the pane you were in. An empty results pane
            // that displaced your file to say "no" is a worse answer than a
            // line of text saying the same thing.
            self.session.status = format!("no matches for {pattern}");
            return;
        }

        let files = Self::files_of(&found.matches);
        let mut report = format!(
            "{} match{} in {files} file{}",
            found.matches.len(),
            if found.matches.len() == 1 { "" } else { "es" },
            if files == 1 { "" } else { "s" },
        );
        if found.capped {
            // Said rather than silently true: a list that stops somewhere and
            // does not say so reads as an answer.
            report.push_str(&format!(" — stopped at {}", crate::find_in_files::LIMIT));
        }
        if found.unreadable > 0 {
            report.push_str(&format!(", {} unreadable", found.unreadable));
        }

        let title = match &scope {
            Some(scope) => format!("find: {pattern} — in {}", scope.display()),
            None => format!("find: {pattern}"),
        };
        self.show_results(crate::results::Results::new(title, query, root, found.matches));
        self.session.status = report;
    }

    /// `:replace /old/new/` — search and offer; `//new/` — offer over the
    /// pane's own matches. Never rewrites on Enter: it **arms** the pane, and
    /// the pane's keys (`a`, `A`, `x`) decide. See
    /// `docs/specs/find-in-files.md`.
    fn run_replace(&mut self, arg: &str, regex: bool) {
        let root = self.tree_root(self.buffer().and_then(|b| b.path.as_deref()));
        let (scope, rest) = Self::split_scope(&root, arg);
        let (pattern, with) = match crate::substitute::parse_replace(rest) {
            Ok(parsed) => parsed,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };

        // An empty pattern is what the pane is showing — the `:s//new/`
        // convention, and how a replace inherits a search already narrowed
        // with `x`. Over a references pane it is a rename.
        if pattern.is_empty() {
            if scope.is_some() {
                self.session.status = "a scope needs a pattern — `:replace src/ /old/new/`".into();
                return;
            }
            let Some(results) = self.window_mut().results_mut() else {
                self.session.status =
                    "no results here — `:find` first, or `:replace /old/new/`".into();
                return;
            };
            results.arm(with);
            let offers = results.pending().len();
            self.session.status = format!(
                "{offers} rewrite{} offered — `a` applies, `A` applies all, `x` drops",
                if offers == 1 { "" } else { "s" },
            );
            return;
        }

        let query = crate::find_in_files::Query {
            pattern,
            regex,
            gitignore: self.options().gitignore,
            ..Default::default()
        };
        let found = match self.search_project(&root, scope.as_deref(), &query) {
            Ok(found) => found,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };
        if found.matches.is_empty() {
            self.session.status = format!("no matches for {}", query.pattern);
            return;
        }

        let files = Self::files_of(&found.matches);
        let mut report = format!(
            "{} rewrite{} in {files} file{} — `a` applies, `A` applies all, `x` drops",
            found.matches.len(),
            if found.matches.len() == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" },
        );
        if found.capped {
            report.push_str(&format!(" — stopped at {}", crate::find_in_files::LIMIT));
        }

        let title = format!("find: {}", query.pattern);
        let mut results = crate::results::Results::new(title, query, root, found.matches);
        results.arm(with);
        self.show_results(results);
        self.session.status = report;
    }

    /// `a` and `A` — applies armed rewrites into buffers, never straight to
    /// disk.
    ///
    /// Every file with an applied match is opened (or reused, so a replace
    /// over a file with unsaved edits edits *those* — the only answer that
    /// cannot lose work), edited as one undo step per file per press, and
    /// left modified. `:wa` commits the lot; `u` in any one file takes it
    /// back. `only` is `a`'s row; `None` is `A`, everything still pending.
    fn apply_replace(&mut self, only: Option<Vec<usize>>) {
        let Some(results) = self.window().results() else { return };
        if results.replace.is_none() {
            self.session.status = "nothing armed — `:replace //new/` offers a rewrite first".into();
            return;
        }
        let indices: Vec<usize> = match only {
            Some(list) => list.into_iter().filter(|&i| !results.is_applied(i)).collect(),
            None => results.pending(),
        };
        if indices.is_empty() {
            self.session.status = "nothing left to apply".into();
            return;
        }

        let root = results.root.clone();
        let with = results.replace.as_ref().expect("checked above").with.clone();
        let query = results.query.clone();
        let hits: Vec<(usize, crate::find_in_files::Match)> =
            indices.iter().map(|&i| (i, results.matches()[i].clone())).collect();

        // The same engine that produced the list, so what gets rewritten is
        // what you were shown — a second engine agreeing today is a second
        // engine that can disagree tomorrow.
        let matcher = match crate::find_in_files::matcher(&query) {
            Ok(matcher) => matcher,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };

        // Grouped by file, and each file's matches applied bottom-up so an
        // earlier rewrite cannot move a later one's line. The results were
        // produced in line order per file, so reversing is enough.
        let mut by_file: Vec<(std::path::PathBuf, Vec<(usize, crate::find_in_files::Match)>)> =
            Vec::new();
        for (index, m) in hits {
            match by_file.last_mut() {
                Some((path, group)) if *path == m.path => group.push((index, m)),
                _ => by_file.push((m.path.clone(), vec![(index, m)])),
            }
        }

        let mut applied: Vec<usize> = Vec::new();
        let (mut changed, mut files, mut skipped, mut failed) = (0usize, 0usize, 0usize, 0usize);
        for (path, group) in by_file {
            let full = root.join(&path);
            let Ok(id) = self.open_path(&full.to_string_lossy()) else {
                failed += 1;
                continue;
            };
            let entry = self.entry_mut(id);
            let before = entry.last.clone();
            let mut here = 0;
            for (index, m) in group.iter().rev() {
                let row = m.line.saturating_sub(1);
                if row >= entry.buffer.line_count() {
                    skipped += 1;
                    continue;
                }
                // Checked against the line as it stands now rather than
                // trusted. The file may have changed since the search — you
                // may have edited it yourself in the meantime — and rewriting
                // a line that no longer says what it said is how a bulk
                // replace eats a repository. A row per line is what makes this
                // check possible at all.
                let line = entry.buffer.line(row);
                if line != m.text {
                    skipped += 1;
                    continue;
                }
                // The replacement is read the way the pattern was: `$1` under
                // `:replace~`, a literal dollar under `:replace`.
                let Some(rewrite) =
                    crate::find_in_files::rewrite_line(&matcher, &line, &with, query.regex)
                else {
                    skipped += 1;
                    continue;
                };
                let start = entry.buffer.rope().line_to_char(row);
                entry.buffer.replace_range(start, start + line.chars().count(), &rewrite.text);
                here += rewrite.count;
                applied.push(*index);
            }
            if here > 0 {
                let after = entry.last.clone();
                entry.buffer.commit_undo(before, after);
                changed += here;
                files += 1;
            }
        }

        // The ✓ is the record of what happened; a skipped line stays pending,
        // visibly, because it is one you need to look at again.
        if let Some(results) = self.window_mut().results_mut() {
            results.mark_applied(&applied);
        }

        let mut report = format!(
            "{changed} replaced in {files} file{} — unwritten, `:wa` to commit",
            if files == 1 { "" } else { "s" }
        );
        if skipped > 0 {
            // Never silent: a line that has moved on since the search is a
            // line you need to look at again, not one to quietly leave.
            report.push_str(&format!(", {skipped} changed since the search"));
        }
        if failed > 0 {
            report.push_str(&format!(", {failed} could not be opened"));
        }
        self.session.status = report;
    }

    /// `:resize` — one amount per axis, applied to the divider you would be
    /// pushing with `Ctrl-W +`.
    ///
    /// Across first and then down, so `:resize 30,10` reports on the width
    /// before the height and a failure names which one could not move. Both
    /// axes are attempted whatever the other did: a layout where the pane can
    /// widen but not grow taller should widen.
    fn run_resize(&mut self, how: crate::resize::Resize) {
        let mut said: Vec<String> = Vec::new();
        // Width is the extent a *vertical* split divides — `:vs` puts panes
        // side by side — and height is a horizontal one's. The names read
        // backwards exactly once, here, and everything downstream is in terms
        // of the split rather than the extent.
        for (amount, axis, name) in
            [(how.x, Dir::Vertical, "width"), (how.y, Dir::Horizontal, "height")]
        {
            let Some(amount) = amount else { continue };
            if let Err(message) = self.resize_axis(amount, axis, name) {
                said.push(message);
            }
        }
        if !said.is_empty() {
            self.session.status = said.join("; ");
        }
    }

    fn resize_axis(
        &mut self,
        amount: crate::resize::Amount,
        axis: Dir,
        name: &str,
    ) -> Result<(), String> {
        use crate::resize::Amount;

        let focus = self.focus;
        let (area, chrome) = (self.area, self.chrome);

        let cells = match amount {
            Amount::By(cells) => cells,
            Amount::Cells(want) => {
                // The pane's own extent, as the frontend last reported it, so
                // `:resize 30` means thirty cells of *text* — the number you
                // can count on screen rather than one that silently includes
                // a border you did not draw.
                let now = match self.window_of(focus) {
                    Some(window) if axis == Dir::Vertical => window.width,
                    Some(window) => window.height,
                    None => return Err(format!("no {name} here")),
                };
                want as i32 - now as i32
            }
            Amount::Ratio(shares) => {
                return match self.layout.ratio(focus, axis, &shares) {
                    Ok(()) => Ok(()),
                    Err(Some(children)) => {
                        Err(format!("{children} panes across that split, so {children} shares"))
                    }
                    Err(None) => Err(format!("nothing to divide the {name} with")),
                };
            }
        };

        if cells == 0 {
            return Ok(());
        }
        match self.layout.resize(focus, axis, cells, area, &chrome) {
            true => Ok(()),
            false => Err(format!("no room to change the {name}")),
        }
    }

    /// `:symbols` — every declaration tree-sitter found, to jump to one.
    ///
    /// Derived from the parse tree that is already there, so it costs a walk
    /// and nothing else: no index, no cache, nothing to invalidate on an edit.
    /// See `docs/specs/symbols.md`.
    fn open_symbol_picker(&mut self) {
        let Some(id) = self.window().buffer() else {
            self.session.status = "no file here".into();
            return;
        };
        let entry = self.entry(id);
        let Some(syntax) = &entry.syntax else {
            // Said rather than shown empty: "no symbols" and "bi cannot read
            // this language" are different answers and only one is your fault.
            self.session.status = "no grammar for this file".into();
            return;
        };
        let found = syntax.symbols(entry.buffer.rope());
        if found.is_empty() {
            self.session.status = "no symbols in this file".into();
            return;
        }

        self.session.symbol_targets = found.iter().map(|s| s.start).collect();
        let items = found
            .into_iter()
            .map(|s| Item { text: format!("{}  {}  {}", s.name, s.kind, s.row + 1), badge: None })
            .collect();
        self.session.picker = Some(Picker::new(PickerKind::Symbol, items, 0));
        self.session.pick_from = Some(std::mem::replace(&mut self.session.mode, Mode::Pick));
    }

    /// `:themes` — every name `Theme::builtins()` gives, no alias among them.
    /// See `docs/specs/theme.md`.
    fn open_theme_picker(&mut self) {
        let names: Vec<&str> = crate::theme::Theme::builtins().collect();
        let current = self.session.options.active_theme(self.remote).to_string();
        let items = names
            .iter()
            .map(|&name| Item { text: name.to_string(), badge: (name == current).then_some('✓') })
            .collect();
        // On the theme already in force, not the front of the list — this is
        // a browse you orient in, not a toggle you reach for repeatedly like
        // the buffer switcher.
        let default_row = names.iter().position(|&name| name == current).unwrap_or(0);
        let mut picker = Picker::new(PickerKind::Theme, items, 0);
        picker.open_on(default_row);
        self.session.picker = Some(picker);
        self.session.pick_from = Some(std::mem::replace(&mut self.session.mode, Mode::Pick));
    }

    fn open_file_picker(&mut self) {
        let root = self.tree_root(self.buffer().and_then(|b| b.path.as_deref()));
        let files = crate::files::walk(&root, crate::files::LIMIT, self.options().gitignore);
        if files.is_empty() {
            // An empty overlay is a worse answer than saying so.
            self.session.status = format!("no files under {}", root.display());
            return;
        }
        let capped = files.len() >= crate::files::LIMIT;
        let items = files.into_iter().map(|text| Item { text, badge: None }).collect();
        // No length floor: a file named `a` is a file.
        self.session.picker = Some(Picker::new(PickerKind::File, items, 0));
        self.session.pick_from = Some(std::mem::replace(&mut self.session.mode, Mode::Pick));
        if capped {
            // Said rather than silently true: a list that stops somewhere has
            // to say where, or the file you cannot find looks like a bug.
            self.session.status =
                format!("more than {} files — showing the first", crate::files::LIMIT);
        }
    }

    /// The fuzzy list over a tree pane, and the one thing that separates its
    /// two keys: `gf` searches the whole tree, `/` searches what is on screen.
    ///
    /// The rows rather than the filesystem is the whole difference from
    /// `Ctrl-P` either way — this moves the selection inside the pane and
    /// opens nothing. Each row is named by its path below the root, so a query
    /// can say which `mod.rs`. See `docs/specs/tree.md`.
    ///
    /// `whole` puts the visible rows at the front of the list and the rest
    /// after them. The picker's sort is stable, so that is how a row already
    /// on screen wins a tie: a better fuzzy match further down still comes out
    /// on top, which is the trade `/` exists for when you want neither.
    fn open_tree_picker(&mut self, whole: bool) {
        let Some(tree) = self.window().tree() else { return };
        let root = tree.root().to_path_buf();
        let paths = match whole {
            true => tree.every_path(crate::files::LIMIT),
            // From row 1: row 0 is the root itself, which has no path below
            // the root to be named by and is where `gg` already goes.
            false => tree.rows()[1..].iter().map(|row| row.path.clone()).collect(),
        };
        let items: Vec<Item> = paths
            .iter()
            .map(|path| Item {
                text: path.strip_prefix(&root).unwrap_or(path).to_string_lossy().into(),
                badge: path.is_dir().then_some('/'),
            })
            .collect();
        if items.is_empty() {
            self.session.status = "nothing in this tree".into();
            return;
        }
        self.session.picker = Some(Picker::new(PickerKind::TreeRow, items, 0));
        self.session.pick_from = Some(std::mem::replace(&mut self.session.mode, Mode::Pick));
    }

    /// Puts the tree's cursor on the row the picker chose, and scrolls to it.
    ///
    /// Through `reveal`, by path, rather than by row index: the whole-tree
    /// list offers rows that are not rows yet, and opening the way down to one
    /// is exactly what `reveal` is. A path already on screen is simply
    /// selected, which is what makes one function serve both keys.
    fn select_tree_row(&mut self, path: String) {
        let height = self.window().height;
        let Some(tree) = self.window_mut().tree_mut() else { return };
        let full = tree.root().join(path);
        tree.reveal(&full);
        tree.scroll_to_selected(height);
    }

    fn open_buffer_picker(&mut self) {
        let ids = self.mru_ids();
        // Relative to the session's root where a path is under it — what you
        // would have typed, and a column of identical prefixes says nothing.
        // The file picker's rule; a path outside the root stays whole.
        let root = self.tree_root(None);
        let items = ids
            .iter()
            .map(|&id| {
                let name = self.name_of(id);
                let text = Path::new(&name)
                    .strip_prefix(&root)
                    .map(|p| p.display().to_string())
                    .unwrap_or(name);
                Item { text, badge: self.is_modified(id).then_some('+') }
            })
            .collect();
        // No length floor: a file named `a` is a file, and hiding it behind
        // `Ctrl-A` is the register ring's problem, not this list's.
        let mut picker = Picker::new(PickerKind::Buffer, items, 0);
        // On the one you were in *before* this one, so a switcher opened and
        // taken is a switch — the same thing `Ctrl-^` does, reached the way
        // you reach everything else.
        picker.open_on(1.min(ids.len().saturating_sub(1)));
        self.session.picker = Some(picker);
        self.session.pick_from = Some(std::mem::replace(&mut self.session.mode, Mode::Pick));
    }

    /// `Ctrl-R` on the `:` line: the picker over the lines you have run.
    ///
    /// The half-typed line becomes the query — it is what you already know
    /// about the command you want — and is what `Esc` gives back.
    fn open_history_picker(&mut self) {
        if self.session.cmd_history.is_empty() {
            // An empty overlay is a worse answer than saying so.
            self.session.status = "no command history".into();
            return;
        }
        let items = self
            .session
            .cmd_history
            .lines()
            .iter()
            .map(|line| Item { text: line.clone(), badge: None })
            .collect();
        let typed = match &self.session.mode {
            Mode::Command(line) => line.to_string(),
            _ => String::new(),
        };
        let mut picker = Picker::new(PickerKind::History, items, 0);
        picker.set_query(typed);
        self.session.picker = Some(picker);
        self.session.pick_from = Some(std::mem::replace(&mut self.session.mode, Mode::Pick));
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
    /// from a `bi .` session, and leaves a session that still has a window.
    fn close_tree(&mut self, id: WindowId) {
        self.park_results(id);
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

    /// A key in a results pane.
    fn run_results_cmd(&mut self, cmd: ResultsCmd, count: usize) {
        let height = self.window().height;

        // The applying pair first: they reach into buffers, which the
        // mutable borrow below could not allow.
        match cmd {
            ResultsCmd::Apply => {
                let Some(results) = self.window().results() else { return };
                let indices = results.selected_indices();
                self.apply_replace(Some(indices));
                if let Some(results) = self.window_mut().results_mut() {
                    // On to the next offer, so `a a a` walks the list.
                    results.advance_to_pending();
                    results.scroll_to_selected(height);
                }
                return;
            }
            ResultsCmd::ApplyAll => {
                self.apply_replace(None);
                return;
            }
            _ => {}
        }

        let Some(results) = self.window_mut().results_mut() else { return };

        match cmd {
            ResultsCmd::Move(by) => results.move_by(by * count as isize),
            ResultsCmd::First => results.select(0),
            ResultsCmd::Last => results.select(usize::MAX),
            ResultsCmd::Remove => {
                results.remove_selected();
                if results.is_empty() {
                    // An empty pane is a worse answer than the line saying
                    // why it is empty.
                    self.session.status = "nothing left in the list".into();
                    let focus = self.focus;
                    self.close_tree(focus);
                    return;
                }
            }
            ResultsCmd::Close => {
                // The pane displaced whatever was here to get on screen, and
                // putting that back is exactly what `close_tree` already does
                // for the other list pane — the last window cannot close, so
                // it shows what it was showing instead.
                let focus = self.focus;
                self.close_tree(focus);
                return;
            }
            ResultsCmd::Apply | ResultsCmd::ApplyAll => unreachable!("handled above"),
            ResultsCmd::Open => {
                let root = results.root.clone();
                let Some(path) = results.selected_path().cloned() else { return };
                // A heading has no line of its own, so it opens the top of the
                // file — which is what you meant by pressing Enter on a file.
                let line = results.selected_match().map(|m| (m.line, m.col));
                let full = root.join(path).to_string_lossy().into_owned();
                self.edit_path(&full);
                if let Some((line, col)) = line {
                    // On the match, not the top of the line: the column is the
                    // whole reason the row was worth showing.
                    self.in_view(|view| {
                        let at = view.buffer.at_row(line.saturating_sub(1), false);
                        let start = view.buffer.rope().line_to_char(view.buffer.row_at(at));
                        let stop = start + view.buffer.line_len(view.buffer.row_at(at));
                        view.goto_char((start + col).min(stop));
                    });
                }
                return;
            }
        }
        if let Some(results) = self.window_mut().results_mut() {
            results.scroll_to_selected(height);
        }
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
        // The option may have moved since the pane opened — `:set gitignore`
        // reaches a living tree through here rather than asking for a reopen.
        let gitignore = self.options().gitignore;
        let Some(tree) = self.window_mut().tree_mut() else { return };
        tree.set_gitignore(gitignore);

        // Everything the tree can answer without leaving itself.
        match cmd {
            TreeCmd::Select { down, count } => return tree.step(down, count),
            TreeCmd::First => return tree.select(0),
            TreeCmd::Last => return tree.select(usize::MAX),
            TreeCmd::HalfPage { down } => return tree.step(down, (height / 2).max(1)),
            TreeCmd::Collapse => return tree.collapse(),
            // The two keys that move the root, and so the two that tell the
            // session where it is scoped from now on.
            TreeCmd::Up | TreeCmd::Down => {
                match cmd {
                    TreeCmd::Up => tree.up(),
                    _ => tree.down(),
                }
                let root = tree.root().to_path_buf();
                self.session.tree_root = Some(root);
                return;
            }
            TreeCmd::Refresh => return tree.refresh(),
            TreeCmd::ToggleHidden => return tree.toggle_hidden(),
            TreeCmd::Find { whole } => return self.open_tree_picker(whole),
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
        let target = self.open_target();
        if let Ok(Some(img)) = self.decode_image(&path) {
            self.show_image(target, img);
            return self.set_focus(target);
        }
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

    /// A new, unnamed, empty buffer in the list.
    ///
    /// Ordinary in every way: `:w` wants a name for it, and the scratch sweep
    /// collects it if you walk away without typing anything.
    fn fresh_scratch(&mut self) -> BufferId {
        let buffer = Buffer::empty();
        let id = self.fresh_buffer_id();
        self.buffers.push(BufferEntry::new(id, buffer));
        self.refresh_git(id);
        self.resolve_options();
        id
    }

    /// Splits the focused window, moves focus into the new one, and hands back
    /// its id. `None` means there was no room, and it has already said so.
    ///
    /// The new window is a clone of the old, so a split lands on the line you
    /// were reading rather than at the top of the file. What it then shows is
    /// the caller's business.
    fn split_focus(&mut self, dir: Dir) -> Option<WindowId> {
        let (focus, area, chrome) = (self.focus, self.area, self.chrome);
        let new = self.fresh_window_id();
        // Beside what you were reading, not on top of it: the new window
        // taking the space the old one occupied is why focus moving into it
        // looked like focus not moving at all.
        if !self.layout.split(focus, new, dir, Place::After, area, &chrome) {
            // Hand the id back rather than leaving a hole in the sequence;
            // nothing depends on it, but a gap invites the question of what
            // used to be there.
            self.next_window -= 1;
            self.session.status = "not enough room to split".into();
            return None;
        }
        let mut window = self.window().clone();
        window.id = new;
        self.windows.push(window);
        self.set_focus(new);
        Some(new)
    }

    /// Opens `tree` as the sidebar: a full-height column down the left of the
    /// screen, `Chrome::tree_width` wide, focused.
    ///
    /// Every key that makes a tree pane comes through here — `Ctrl-W e` and
    /// `:vs .` alike — so the two cannot drift apart about where a tree lives.
    /// `None` means there was no room, and it has already said so.
    fn open_tree_pane(&mut self, tree: Tree) -> Option<WindowId> {
        let (area, chrome) = (self.area, self.chrome);
        let new = self.fresh_window_id();
        // The root rather than the focused window. A file tree is a column of
        // the *screen*: which pane you happened to press the key in is not a
        // fact about where a sidebar belongs, and splitting that pane put the
        // tree in a different place every time.
        if !self.layout.split_root(new, Dir::Vertical, Place::Before, area, &chrome) {
            // Hand the id back rather than leaving a hole in the sequence.
            self.next_window -= 1;
            self.session.status = "not enough room to split".into();
            return None;
        }
        self.windows.push(Window::showing(new, Content::Tree(tree)));
        self.set_focus(new);

        // A half-screen tree is not a sidebar. Narrowed to the width the
        // frontend asked for, which becomes a share of the terminal from here
        // on, like every other pane.
        let width = self
            .layout
            .rect_of(new, area, &chrome)
            .map_or(0, |rect| rect.width)
            .saturating_sub(chrome.tree_width);
        if width > 0 {
            self.layout.resize(new, Dir::Vertical, -(width as i32), area, &chrome);
        }
        Some(new)
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
                let buffer = match path.as_deref() {
                    None => None,
                    // A directory is a tree, and a tree is a sidebar wherever
                    // it was asked for: down the left of the screen rather
                    // than beside this pane, and never in the direction `:sp`
                    // named. See `docs/specs/tree.md`.
                    Some(path) if std::path::Path::new(path).is_dir() => {
                        match Tree::new(path, self.options().gitignore) {
                            Ok(tree) => {
                                self.open_tree_pane(tree);
                            }
                            Err(e) => self.session.status = format!("{e:#}"),
                        }
                        return;
                    }
                    Some(path) => {
                        // An image splits like anything else, decoded before
                        // the split for the same reason the buffer is opened
                        // before it: a failure must not leave a pane showing
                        // the wrong thing.
                        if let Ok(Some(img)) = self.decode_image(std::path::Path::new(path)) {
                            let Some(new) = self.split_focus(dir) else { return };
                            self.show_image(new, img);
                            return;
                        }
                        match self.open_path(path) {
                            Ok(id) => Some(id),
                            Err(e) => {
                                self.session.status = format!("{e:#}");
                                return;
                            }
                        }
                    }
                };

                let Some(new) = self.split_focus(dir) else { return };
                // Through `show`, so the new window records where the
                // duplicated one was before it moves off that buffer.
                if let Some(id) = buffer {
                    self.show(new, id);
                }
            }

            // `:new`, `:vnew`, `:enew`. Not a spelling of `:split`: vim's
            // `new` forms land on an *empty* buffer, and shipping them as
            // aliases gave anyone who typed one a second view of the file they
            // already had, silently. `None` is `:enew` — this window, no split.
            WindowCmd::New { dir } => {
                let target = match dir {
                    Some(dir) => match self.split_focus(dir) {
                        Some(new) => new,
                        None => return,
                    },
                    None => focus,
                };
                let id = self.fresh_scratch();
                self.show(target, id);
            }

            WindowCmd::Pick => self.pick_window(),

            WindowCmd::Tree => {
                // One tree, so the key that opened it is the key that puts it
                // away. Two trees are two of the same thing, and the second is
                // never the one you wanted.
                if let Some(open) = self.tree_window() {
                    return self.close_tree(open);
                }

                // Read before the split, because which file you are in is a
                // fact about the window you are leaving rather than the one
                // being made.
                let path = self.buffer().and_then(|b| b.path.clone());
                let root = self.tree_root(path.as_deref());
                let tree = match Tree::new(&root, self.options().gitignore) {
                    Ok(tree) => tree,
                    Err(e) => {
                        self.session.status = format!("{e:#}");
                        return;
                    }
                };
                self.session.tree_root = Some(tree.root().to_path_buf());

                if self.open_tree_pane(tree).is_none() {
                    return;
                }
                self.reveal_in_tree(path.as_deref());
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
        // A results pane goes down with its window; keep it for `:results`.
        self.park_results(id);
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
        // Closing the only window that showed a blank is the other way one
        // becomes unreachable.
        self.sweep_scratch();
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
    /// run anything in. Without this `:q` cannot leave `bi .`.
    ///
    /// Returns whether it handled the action.
    /// The normal keymap, re-read as pixels, for a window holding an image.
    ///
    /// No `KeyMode::Image` and no bindings of its own — closing an image and
    /// jumping out of its window are the keys you already press, because the
    /// window is an ordinary window. Motions move the crop; the mode-entering
    /// commands that live on the session are swallowed, because a search line
    /// over a picture is a promise the pane cannot keep; everything else
    /// falls through to what it already did. See `docs/specs/images.md`.
    fn run_image_action(&mut self, cmd: &Command) -> bool {
        let count = cmd.count.max(1);
        let steps = count as i64;
        let Some(img) = self.window_mut().img_mut() else { return false };
        match &cmd.action {
            Action::Move(Motion::Left) => img.step_by(-steps, 0),
            Action::Move(Motion::Right) => img.step_by(steps, 0),
            Action::Move(Motion::Up) => img.step_by(0, -steps),
            Action::Move(Motion::Down) => img.step_by(0, steps),
            Action::Move(Motion::FirstLine) => img.to_edge_y(true),
            Action::Move(Motion::LastLine | Motion::Line(_)) => img.to_edge_y(false),
            Action::Move(Motion::LineStart | Motion::FirstNonBlank) => img.to_edge_x(true),
            Action::Move(Motion::LineEnd | Motion::LastNonBlank) => img.to_edge_x(false),
            Action::ScrollHalfPage { down } => img.half_page(*down, count),
            Action::ScrollLine { down } => img.step_by(0, if *down { steps } else { -steps }),
            // Swallowed rather than run: each of these moves the session into
            // a mode that reads the buffer this window does not have.
            Action::EnterFind
            | Action::EnterSearch { .. }
            | Action::SearchWord { .. }
            | Action::ShowScopes
            | Action::EnterVisual(_) => {}
            _ => return false,
        }
        true
    }

    fn run_session_action(&mut self, action: &Action) -> bool {
        match action {
            Action::EnterCommandMode => {
                self.session.status.clear();
                // The selection comes with you, and says so. The prefill is
                // the whole of why that is right: what is about to be acted on
                // is visible before you commit, it is editable when you meant
                // something else, and it is the scope language you already
                // have rather than a second invisible rule about when a
                // command silently means the selection.
                //
                // `'v` rather than vim's `'<,'>`, because `'<,'>` can only say
                // *rows* — and a rectangle acted on as rows is the surprise
                // the prefill exists to prevent. `'<,'>` still parses, and
                // typing it over the prefill is how you ask for the rows.
                self.session.interrupted_visual = self.session.visual();
                let line = match self.session.interrupted_visual {
                    Some(_) => CmdLine::from("'v "),
                    None => CmdLine::default(),
                };
                self.session.mode = Mode::Command(line);
            }
            Action::CommandChar(c) => {
                if let Mode::Command(line) = &mut self.session.mode {
                    line.insert(*c);
                }
            }
            Action::CommandBackspace => {
                // Nothing to delete on an empty line is how `Backspace` leaves;
                // nothing to delete at column 0 of a line with text on it is
                // just nothing to delete.
                if let Mode::Command(line) = &mut self.session.mode
                    && !line.backspace()
                    && line.is_empty()
                {
                    self.session.mode = Mode::Normal;
                    // Leaving this way is `Esc` spelled slower.
                    let shape = self.session.interrupted_visual.take();
                    self.revive_visual(shape);
                }
            }
            Action::CommandMove(how) => {
                if let Mode::Command(line) = &mut self.session.mode {
                    match how {
                        CmdMove::Left => line.left(),
                        CmdMove::Right => line.right(),
                        CmdMove::Home => line.home(),
                        CmdMove::End => line.end(),
                    }
                }
            }
            Action::CommandRecall { older } => {
                // The store and the line are both on the session, so this is
                // the one place that can hand one to the other.
                let history = self.session.cmd_history.lines().to_vec();
                if let Mode::Command(line) = &mut self.session.mode {
                    line.recall(&history, *older);
                }
            }
            Action::CommandCancel => {
                self.session.mode = Mode::Normal;
                // The one `:` line that means something when abandoned.
                self.abandon_paste();
                // Backing out of the `:yname ` prompt must not lose the text —
                // it goes where an unnamed capture always goes.
                if let Some(entry) = self.session.pending_named.take() {
                    self.session.registers.push(entry);
                    self.session.status = "not named — kept on the ring".into();
                }
                // Backing out of the line is not a reason to lose what you
                // had selected before pressing `:`.
                let shape = self.session.interrupted_visual.take();
                self.revive_visual(shape);
            }
            Action::CommandExecute => {
                let Mode::Command(line) = std::mem::take(&mut self.session.mode) else {
                    return true;
                };
                // Taken with the line, and handed to the command as an
                // argument. Both die here together, so neither can go stale
                // and neither can be missing.
                let shape = self.session.interrupted_visual.take();
                // Before running it, so a command that failed is the one you
                // can recall and fix — which is most of what a history is for.
                // Only what was typed here: an `Ex` action is a keybinding or
                // an internal caller, and a history of lines you never typed is
                // noise in the list that exists to give your own back.
                self.session.cmd_history.push(&line);
                self.run_ex_over(&line, shape);
                self.revive_visual(shape);
                // `:yname` consumed it; any other line typed over the prompt
                // leaves it, and it goes to the ring rather than lingering
                // into the next capture.
                if let Some(entry) = self.session.pending_named.take() {
                    self.session.registers.push(entry);
                }
            }
            Action::BoundaryJump { forward } => self.boundary_jump(*forward),
            Action::Ex { line, run } => {
                let line = line.clone();
                match run {
                    true => self.run_ex(&line),
                    false => self.session.mode = Mode::Command(line.into()),
                }
            }

            // Here rather than in the view, beside the `:` line it is opened
            // from: both have to work in a window holding a tree, where there
            // is no rope to run anything against.
            Action::OpenPicker(PickerKind::History) => self.open_history_picker(),
            Action::OpenPicker(PickerKind::File) => self.open_file_picker(),
            Action::OpenPicker(PickerKind::Symbol) => self.open_symbol_picker(),

            // The same reason again: a letter can be sitting on a tree pane,
            // and pressing it changes which window is focused — which is a
            // fact about the session and not about any rope.
            Action::LabelChar(c) => self.label_key(Some(*c)),
            Action::LabelCancel => self.label_key(None),
            Action::ShowScopes => self.show_scopes(),
            Action::EnterFind => self.enter_find(),
            Action::FindChar(c) => self.find_key(*c),
            Action::FindBackspace => self.find_backspace(),
            Action::FindCancel => self.end_find(),

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

    /// Cancelling gives back the mode the picker was opened from, so `"p` over
    /// a selection and then `Esc` leaves the selection where it was rather
    /// than silently ending it, and `Ctrl-R` over a half-typed `:` line gives
    /// the line back rather than eating it.
    fn close_picker(&mut self) {
        self.session.picker = None;
        self.session.mode = self.session.pick_from.take().unwrap_or(Mode::Normal);
    }

    /// Runs whatever the highlighted row meant.
    ///
    /// The kinds part company here: a buffer pick reaches the list, which is
    /// the editor's; a register pick pastes, which needs a view — and a tree
    /// pane has nothing to paste into; and a history pick runs nothing at all,
    /// because the line is going back to the command line to be edited.
    fn accept_pick(&mut self) {
        let picker = self.session.picker.take();
        // The selection the picker was opened over is still there, and the
        // paste is about to consume it — so the mode goes back to what it was
        // for `paste_pick` to read, and the paste itself ends visual mode.
        self.session.mode = self.session.pick_from.take().unwrap_or(Mode::Normal);
        let Some(picker) = picker else { return };
        // Nothing highlighted is nothing to accept, which leaves you exactly
        // where cancelling would.
        let Some(chosen) = picker.selected() else { return };
        match picker.kind {
            // A position rather than an id, because the rows were built from
            // the list in order and it cannot change while the picker holds
            // every key.
            PickerKind::Buffer => self.run_buffer_cmd(BufferCmd::Chosen(chosen)),
            // Through `:e`, like everything else that opens a file, so one
            // already open comes back as the buffer it is rather than as a
            // second copy of it.
            PickerKind::File => {
                let path = picker.items()[chosen].text.clone();
                let root = self.tree_root(None);
                let full = root.join(path).to_string_lossy().to_string();
                self.edit_path(&full);
            }
            // A row rather than a file: this one moves the tree's cursor and
            // opens nothing, which is why it is not `PickerKind::File`.
            PickerKind::TreeRow => {
                let path = picker.items()[chosen].text.clone();
                self.select_tree_row(path);
            }
            // Also a row rather than a file, and for the same reason: the list
            // came out of the buffer already in front of you.
            PickerKind::Symbol => {
                // Bounds-checked rather than indexed: the list is rebuilt on
                // every `:symbols`, and a stale index is a panic where a
                // no-op will do.
                if let Some(&at) = self.session.symbol_targets.get(chosen) {
                    self.in_view(|view| view.goto_char(at));
                }
            }
            PickerKind::Register { before } => {
                self.in_view(|view| view.paste_pick(chosen, before));
            }
            PickerKind::Named { before } => {
                self.in_view(|view| view.paste_named(chosen, before));
            }
            // Put back on the `:` line, unrun. Editing it is the point: the
            // line you reach for a history for is the one with a word wrong.
            PickerKind::History => {
                let line = picker.items()[chosen].text.clone();
                self.session.mode = Mode::Command(line.into());
            }
            // `:set theme <name>`, exactly — so a re-resolve, the status
            // line, and which of `theme` / `ssh_theme` moved are all the
            // same thing `:set` already gets right.
            PickerKind::Theme => {
                let name = picker.items()[chosen].text.clone();
                self.set_option(&format!("theme {name}"));
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
            let trim = entry.options.trim;
            if trim.does_anything() {
                let edits = entry.buffer.trim(&trim);
                let across = |at: usize| edits.iter().fold(at, |at, edit| edit.map(at));
                entry.last = entry.last.iter().map(|&(a, h)| (across(a), across(h))).collect();
            }
            // No selections to record for a buffer nobody is looking at, and
            // the ones for a buffer in view have not moved.
            let pairs = entry.last.clone();
            match entry.buffer.save(pairs.clone(), pairs) {
                Ok(()) => {
                    written += 1;
                    self.session.pending_saves.push(*id);
                }
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
        // you remember which command a directory wants. A directory re-roots
        // the pane you are in, tree or not — that is what naming one means.
        if std::path::Path::new(path).is_dir() {
            return self.show_tree(path);
        }
        let target = self.open_target();
        let fallback = match self.decode_image(std::path::Path::new(path)) {
            Ok(Some(img)) => {
                self.show_image(target, img);
                return self.set_focus(target);
            }
            Ok(None) => None,
            Err(why) => Some(why),
        };
        match self.open_path(path) {
            Ok(id) => {
                self.show(target, id);
                self.set_focus(target);
                self.session.status = match fallback {
                    Some(why) => format!("\"{}\" opened as text — {why}", self.name_of(id)),
                    None => format!("\"{}\" loaded", self.name_of(id)),
                };
            }
            Err(e) => self.session.status = format!("{e:#}"),
        }
    }

    /// Which window a file being opened should appear in.
    ///
    /// This one, unless it is a tree — a sidebar you looked a file up in is a
    /// sidebar you still want, so the file goes where Enter on a tree row
    /// sends it and focus follows it there. One rule for `Ctrl-P`, `:e` and
    /// the tree's own Enter. See `docs/specs/tree.md`.
    fn open_target(&self) -> WindowId {
        match self.window().tree().is_some() {
            true => self.handoff_window(),
            false => self.focus,
        }
    }

    /// `path` decoded, when it is an image at all.
    ///
    /// `Ok(None)` is "not an image — this was never for you"; `Err` is a
    /// decode failure worth telling, and the caller opens the bytes as the
    /// text they always were. See `docs/specs/images.md`.
    fn decode_image(&mut self, path: &Path) -> std::result::Result<Option<Img>, String> {
        if !crate::img::looks_like_image(path) {
            return Ok(None);
        }
        match Img::open(path, self.next_image) {
            Ok(img) => {
                self.next_image += 1;
                Ok(Some(img))
            }
            Err(e) => Err(format!("{e:#}")),
        }
    }

    /// Points `window` at an image, parking what it held — the same
    /// bookkeeping `show_tree` does, for the same reasons: the buffer being
    /// left records where this window was in it, and `Ctrl-^` brings the
    /// image back with its crop intact because `alt` carries whole contents.
    fn show_image(&mut self, window: WindowId, img: Img) {
        if let Some(text) = self.window_of(window).and_then(Window::text) {
            let (buffer, pairs) = (text.buffer, text.selections.as_pairs());
            self.entry_mut(buffer).last = pairs;
        }
        let name = img
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| img.path.display().to_string());
        self.session.status = format!("\"{name}\" {}×{}", img.width, img.height);
        self.park_results(window);
        if let Some(window) = self.window_mut_of(window) {
            window.show(Content::Image(img));
        }
        self.sweep_scratch();
    }

    /// `:bd` on an image window: the alternate comes back when it still
    /// exists, the most recent buffer otherwise. The image itself is
    /// discarded, alternate included, because that is what delete means.
    fn dismiss_image(&mut self, window: WindowId) {
        let alt = self.window_mut_of(window).and_then(|w| w.alt.take());
        // A parked buffer may have been `:bd`-ed since it was parked; showing
        // it back would resurrect a dead id.
        let alive = match &alt {
            Some(Content::Text(text)) => self.buffers.iter().any(|b| b.id == text.buffer),
            Some(_) => true,
            None => false,
        };
        if alive && let Some(w) = self.window_mut_of(window) {
            w.content = alt.expect("checked above");
            return;
        }
        if let Some(&id) = self.mru_ids().first() {
            self.show(window, id);
            // `show` parked the image as the alternate; delete discards.
            if let Some(w) = self.window_mut_of(window) {
                w.alt = None;
            }
        }
    }

    /// The image a window shows, mutably. The frontend reports the room it
    /// gave the pane through this, the way `size_window` reports rows.
    pub fn image_pane_mut(&mut self, id: WindowId) -> Option<&mut Img> {
        self.window_mut_of(id)?.img_mut()
    }

    /// An image by its stable id, wherever it is showing — what a frontend
    /// that uploads pixels once looks them up by.
    pub fn image_with_id(&self, id: u64) -> Option<&Img> {
        self.windows.iter().find_map(|w| w.img().filter(|img| img.id == id))
    }

    /// Points the focused window at a tree on `root`, parking what it held.
    fn show_tree(&mut self, root: &str) {
        let tree = match Tree::new(root, self.options().gitignore) {
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
        // Naming a directory is one of the three ways to say where the session
        // is scoped — `+` and `-` are the others.
        self.session.tree_root = Some(tree.root().to_path_buf());
        let focus = self.focus;
        self.park_results(focus);
        self.window_mut().show(Content::Tree(tree));
    }

    /// `-` from a text window: the tree on the session's root, with the file
    /// you were in revealed.
    ///
    /// The root, not this file's directory. Opening `pkg/a.rs` and coming back
    /// out must land where you were scoped, not one level above the file that
    /// happened to be open — otherwise every trip through a file walks the
    /// root somewhere you never asked for.
    fn show_tree_here(&mut self) {
        let path = self.buffer().and_then(|b| b.path.clone());
        let root = self.tree_root(path.as_deref());

        self.show_tree(&root.to_string_lossy());
        self.reveal_in_tree(path.as_deref());
    }

    /// Where a tree opens: the session's root, or — the first time, with no
    /// root ever set — the directory holding `path`, and failing that the
    /// working directory. A `[No Name]` buffer has no directory, which is the
    /// case that reaches the end.
    fn tree_root(&self, path: Option<&Path>) -> PathBuf {
        if let Some(root) = &self.session.tree_root {
            return root.clone();
        }
        match path.and_then(Path::parent) {
            Some(parent) => parent.to_path_buf(),
            None => std::env::current_dir().unwrap_or_default(),
        }
    }

    /// Opens the focused tree down to `path` and puts the selection on it.
    ///
    /// What makes `-` and `Ctrl-W e` land where you were rather than at the
    /// top of the directory. It has to open the way down as well as select,
    /// because the root is the session's now and the file may sit several
    /// directories below it.
    fn reveal_in_tree(&mut self, path: Option<&Path>) {
        let (Some(path), Some(tree)) = (path, self.window_mut().tree_mut()) else { return };
        tree.reveal(path);
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
                entry.syntax = syntax_for(&entry.buffer, &entry.options);
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
        self.session.registers.push(Entry { text: path.clone(), kind: Shape::Chars });
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
        let queue: Vec<Mark> = self.session.clipboard.marks().to_vec();

        // A directory cannot hold itself. That is a loop rather than a
        // mistake, and no name for the destination would fix it.
        if let Some(mark) = queue.iter().find(|mark| into.starts_with(&mark.path)) {
            self.session.status = format!("cannot paste \"{}\" into itself", mark.path.display());
            return;
        }

        self.session.pasting = Some(Pasting { queue, into: into.to_path_buf(), done: 0 });
        self.run_paste(None);
    }

    /// Places what is queued until something is in the way.
    ///
    /// `rename` is the destination for the head of the queue when the command
    /// line has just supplied one; otherwise each lands under its own name.
    fn run_paste(&mut self, rename: Option<std::path::PathBuf>) {
        let Some(mut pasting) = self.session.pasting.take() else { return };
        let mut rename = rename;

        while let Some(mark) = pasting.queue.first().cloned() {
            let source = mark.path;
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
                self.session.mode = Mode::Command(format!("paste-as {}", target.display()).into());
                self.session.pasting = Some(pasting);
                return;
            }

            // Each path's own verb, so a mixed set does both in one pass.
            let result = match mark.mode {
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

        // The cuts are spent — their sources are not there any more — and the
        // copies are not, so the same files can go to a second place. With a
        // mixed set that is a partial clear rather than a choice between the
        // two: what survives is exactly what still exists.
        self.session.clipboard.clear_cuts();
        self.session.status = format!("{} pasted into {}", pasting.done, pasting.into.display());
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
        self.session.mode = Mode::Command(line.into());
    }

    /// Runs a `:` line.
    ///
    /// Parsed before anything is dispatched, rather than discovered inside a
    /// view: a tree window has no view for a `:` line to land in, and the
    /// window and buffer commands no longer have to travel back out as
    /// escalations to reach the lists they change. See `docs/specs/tree.md`.
    pub fn run_ex(&mut self, line: &str) {
        self.run_ex_over(line, self.session.mode.visual());
    }

    /// The same, over a selection whose shape is named rather than looked up.
    ///
    /// The shape is an argument because the mode that held it is gone by the
    /// time a command runs: [`Action::CommandExecute`] takes the `:` line out
    /// of the session before dispatching, and a command that asked the mode
    /// afterwards would be told there is no selection. That is exactly how a
    /// rectangle used to reach `:case` as a set of whole lines.
    ///
    /// See `docs/specs/regions.md`.
    fn run_ex_over(&mut self, line: &str, shape: Option<Shape>) {
        let Some(parsed) = parse_ex(line) else { return };
        match parsed {
            ExLine::Window(cmd) => self.run_window_cmd(cmd),
            ExLine::Buffer(cmd) => self.run_buffer_cmd(cmd),
            ExLine::Edit { path } => self.edit_path(&path),
            ExLine::Quit { force } => self.quit(force),
            ExLine::QuitAll { force } => self.quit_all(force),
            ExLine::WriteAll => self.write_all(),
            ExLine::Highlight(on) => self.session.options.hlsearch = on,
            ExLine::Whitespace(on) => self.set_whitespace(on),
            ExLine::Set(arg) => self.set_option(&arg),
            ExLine::Name { scope, name } => {
                // The capture the `"n` prompt is holding wins a bare
                // `:yname` — that is the prompt flow. A range, a selection,
                // or nothing held yanks the region the way `:d` deletes it.
                if scope.is_none() && shape.is_none() && self.session.pending_named.is_some() {
                    self.name_pending(&name);
                } else {
                    self.in_view(|view| view.yank_named(scope, shape, &name));
                }
            }
            ExLine::Create(path) => self.create_path(&path),
            ExLine::Rename { from, to } => self.rename_path(&from, &to),
            ExLine::Delete { path, force } => self.delete_path(&path, force),
            ExLine::Paste(dir) => match dir {
                Some(dir) => self.paste_into(std::path::Path::new(&dir)),
                None => self.paste_into_selected(),
            },
            ExLine::PasteAs(path) => self.run_paste(Some(path.into())),
            ExLine::Move { scope, to } => {
                self.in_view(|view| view.move_to(scope, shape, to));
            }
            ExLine::Sort { scope, how } => {
                self.in_view(|view| view.sort_rows(scope, shape, &how));
            }
            ExLine::Case { scope, style } => {
                self.in_view(|view| view.recase(scope, shape, style));
            }
            ExLine::Retab(scope) => {
                self.in_view(|view| view.retab(scope, shape));
            }
            ExLine::Substitute { scope, how } => self.run_substitute(scope, shape, &how),
            ExLine::SubstituteRepeat { scope } => match self.session.last_substitute.clone() {
                Some(how) => self.run_substitute(scope, shape, &how),
                None => self.session.status = "no substitute to repeat".into(),
            },
            ExLine::Global { scope, invert, pattern, cmd } => {
                self.run_global(scope, shape, invert, &pattern, &cmd);
            }
            ExLine::Normal { scope, keys } => self.run_normal_cmd(scope, shape, &keys),
            ExLine::DeleteLines { scope } => {
                self.in_view(|view| view.delete_rows(scope, shape));
            }
            ExLine::Alternate => self.open_alternate(),
            ExLine::Symbols => self.open_symbol_picker(),
            ExLine::Themes => self.open_theme_picker(),
            ExLine::Lsp(cmd) => self.run_lsp(cmd),
            ExLine::Definition => self.lsp_goto(lsp::Goto::Definition),
            ExLine::Declaration => self.lsp_goto(lsp::Goto::Declaration),
            ExLine::Implementation => self.lsp_goto(lsp::Goto::Implementation),
            ExLine::Peek => self.peek_definition(),
            ExLine::TsMarks => self.toggle_ts_marks(),
            ExLine::TsSplit => self.ts_split(),
            ExLine::TsJoin => self.ts_join(),
            ExLine::Zen => self.toggle_zen(),
            ExLine::Diags => self.show_diagnostics(),
            ExLine::References => self.lsp_references(),
            ExLine::Format => self.lsp_format(),
            ExLine::DiagnosticJump { forward } => self.diagnostic_jump(forward),
            ExLine::Hover => self.lsp_hover(),
            ExLine::Resize(how) => self.run_resize(how),
            ExLine::Find { pattern, regex } => self.run_find(&pattern, regex),
            ExLine::Replace { arg, regex } => self.run_replace(&arg, regex),
            ExLine::ResultsPane => self.reopen_results(),
            ExLine::Unknown(name) => self.session.status = format!("not a command: {name}"),
            ExLine::Error(message) => self.session.status = message,
            ExLine::ReloadConfig => self.reload_config(),

            // The rest need the rope, and so need a view.
            ExLine::Write(path) => {
                self.in_view(|view| view.write(&path));
            }
            ExLine::Revert { force } => {
                self.in_view(|view| view.edit(force));
                // The file on disk moved under the buffer; its standing with
                // the index may have moved the same way.
                if let Some(id) = self.window().buffer() {
                    self.refresh_git(id);
                }
            }
            ExLine::Goto(address) => {
                self.in_view(|view| view.goto(address));
            }
            ExLine::WriteQuit(path) => {
                if self.in_view(|view| view.write(&path)) == Some(true) {
                    self.quit(true);
                }
            }
        }
    }

    /// `:s` and the `&` family both end here: run it, and remember what ran.
    ///
    /// The memory holds the pattern that was in force, not the empty one that
    /// may have been typed — `&` repeats what happened, and what happened to
    /// an empty pattern was the last search *of that moment*.
    fn run_substitute(
        &mut self,
        scope: Option<Scope>,
        shape: Option<Shape>,
        how: &crate::substitute::Substitute,
    ) {
        // The last search is the session's, and an empty pattern means it —
        // so it is read here, where both are in reach.
        let last = self.session.last_search.as_ref().map(|s| s.pattern.clone());
        let found = self.in_view(|view| view.substitute(scope, shape, how, last));
        if let Some(Some(pattern)) = found {
            self.session.last_search =
                Some(Search { pattern: pattern.clone(), whole_word: false, forward: true });
            self.session.last_substitute =
                Some(crate::substitute::Substitute { pattern, ..how.clone() });
        }
    }

    /// Runs `run` with the focused buffer's undo deferred into one group —
    /// the batch commands' single `u`. Begun and ended on the buffer that was
    /// focused at the start, whatever `run` did with focus in between.
    fn grouped_undo(&mut self, run: impl FnOnce(&mut Self)) {
        let id = self.window().buffer();
        let before = self.selections().map(|s| s.as_pairs());
        let (Some(id), Some(before)) = (id, before) else {
            run(self);
            return;
        };
        self.entry_mut(id).buffer.begin_undo_group(before);
        run(self);
        let after = self.selections().map(|s| s.as_pairs()).unwrap_or_default();
        self.entry_mut(id).buffer.end_undo_group(after);
    }

    /// `:[range]g/pattern/cmd` — see `docs/specs/global.md`.
    fn run_global(
        &mut self,
        scope: Option<Scope>,
        shape: Option<Shape>,
        invert: bool,
        pattern: &str,
        cmd: &str,
    ) {
        // The sub-command is judged before anything runs: refused by name,
        // not discovered broken on the fortieth matching line.
        if cmd.is_empty() {
            self.session.status = "and do what? `:g/pattern/d`".into();
            return;
        }
        match parse_ex(cmd) {
            Some(
                ExLine::DeleteLines { .. }
                | ExLine::Substitute { .. }
                | ExLine::SubstituteRepeat { .. }
                | ExLine::Move { .. }
                | ExLine::Case { .. }
                | ExLine::Retab(_)
                | ExLine::Normal { .. },
            ) => {}
            Some(ExLine::Error(message)) => {
                self.session.status = message;
                return;
            }
            // A whitelist, not a blacklist: the failure mode of a blacklist
            // is `:g/x/q` closing the editor on the first match. The allowed
            // commands are the ones that act on the cursor's line when no
            // range narrows them, which is exactly the contract the walk
            // below hands each of them.
            _ => {
                let head = cmd.split_whitespace().next().unwrap_or(cmd);
                self.session.status =
                    format!("`:g` runs d, s, &, m, case, retab or normal — not `{head}`");
                return;
            }
        }

        let pattern = match pattern.is_empty() {
            false => pattern.to_string(),
            // An empty pattern is the last search, the same rule `:s` reads
            // it by.
            true => match self.session.last_search.as_ref().map(|s| s.pattern.clone()) {
                Some(pattern) if !pattern.is_empty() => pattern,
                _ => {
                    self.session.status = "no previous search".into();
                    return;
                }
            },
        };

        // The scan finishes before the first command runs, so a command
        // cannot edit a line into or out of the match set.
        let Some(rows) = self.in_view(|view| view.global_rows(scope, shape, &pattern, invert))
        else {
            return;
        };
        let rows = match rows {
            Ok(rows) => rows,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };
        if rows.is_empty() {
            self.session.status = format!("pattern not found: {pattern}");
            return;
        }
        // The pattern becomes the last search, as `:s`'s does — it is what
        // makes `:g/foo/s//bar/g` the idiom it is.
        self.session.last_search =
            Some(Search { pattern: pattern.clone(), whole_word: false, forward: true });

        let matched = rows.len();
        self.grouped_undo(|ed| {
            // A command that deletes or adds lines shifts every row below
            // it; the walk carries the difference. Exact for commands that
            // stay on their own line — which the allowed ones do.
            let mut delta: isize = 0;
            for row in rows {
                let row = row as isize + delta;
                let Some(buffer) = ed.buffer() else { break };
                let lines_before = buffer.rope().len_lines() as isize;
                if row < 0 || row >= lines_before {
                    continue;
                }
                let at = buffer.rope().line_to_char(row as usize);
                ed.set_cursor(Cursor::at(at));
                ed.run_ex_over(cmd, None);
                let Some(buffer) = ed.buffer() else { break };
                delta += buffer.rope().len_lines() as isize - lines_before;
            }
        });

        self.session.status =
            format!("{matched} matching line{}", if matched == 1 { "" } else { "s" });
        self.session.mode = Mode::Normal;
    }

    /// `:[range]normal {keys}` — see `docs/specs/global.md`.
    fn run_normal_cmd(&mut self, scope: Option<Scope>, shape: Option<Shape>, keys: &str) {
        if self.replaying_normal {
            self.session.status = "normal does not nest".into();
            return;
        }
        let rows = match scope {
            None => None,
            Some(_) => match self.in_view(|view| view.scope_rows(scope, shape)) {
                None => return,
                Some(Err(message)) => {
                    self.session.status = message;
                    return;
                }
                Some(Ok((first, last))) => Some((first, last)),
            },
        };

        self.replaying_normal = true;
        self.grouped_undo(|ed| match rows {
            None => ed.feed_keys(keys),
            Some((first, last)) => {
                let mut delta: isize = 0;
                for row in first..=last {
                    let row = row as isize + delta;
                    let Some(buffer) = ed.buffer() else { break };
                    let lines_before = buffer.rope().len_lines() as isize;
                    if row < 0 || row >= lines_before {
                        continue;
                    }
                    let at = buffer.rope().line_to_char(row as usize);
                    ed.set_cursor(Cursor::at(at));
                    ed.feed_keys(keys);
                    let Some(buffer) = ed.buffer() else { break };
                    delta += buffer.rope().len_lines() as isize - lines_before;
                }
            }
        });
        self.replaying_normal = false;
        self.session.mode = Mode::Normal;
    }

    /// Feeds characters through the same key grammar a frontend uses, then
    /// puts the editor back in normal mode — the trailing `Esc` nobody can
    /// type on a `:` line, pressed for you so a half-finished insert cannot
    /// leak into the next line of a `:g`.
    fn feed_keys(&mut self, keys: &str) {
        let mut input = crate::input::Input::default();
        for c in keys.chars() {
            let key = crate::key::Key::char(c);
            if let Some(cmd) = input.on_key(key, &self.session.mode, self.content_kind()) {
                self.apply(cmd);
            }
        }
        // Twice at most: one Esc closes an insert or a selection, and one
        // more closes what that revealed — a `:` line typed under insert.
        for _ in 0..2 {
            if self.session.mode == Mode::Normal {
                break;
            }
            let esc = crate::key::Key::code(crate::key::KeyCode::Esc);
            if let Some(cmd) = input.on_key(esc, &self.session.mode, self.content_kind()) {
                self.apply(cmd);
            }
        }
    }

    /// Puts the editor back in the visual mode a `:` line interrupted, when
    /// the selection it named is still standing.
    ///
    /// One test, not a registry of which commands fail: a command that
    /// consumed the selection collapsed it as part of its edit (`:'v case`,
    /// `:'v s/…`), and a collapsed selection is nothing to revive. Everything
    /// else — a parse error, `no line 99`, a command with no use for the
    /// scope, `Esc` on the line, `:'v m +1` deliberately keeping the moved
    /// block — leaves the selection with room in it, and the renderer paints
    /// every uncollapsed selection whatever the mode says. Without this the
    /// paint lied: the selection was on screen but the next `:` prefilled
    /// nothing, and the retyped command quietly acted on the word under the
    /// cursor instead. See `docs/specs/cmdline.md`.
    fn revive_visual(&mut self, shape: Option<Shape>) {
        let Some(shape) = shape else { return };
        if self.session.mode != Mode::Normal {
            // The command opened something of its own — another `:` line, a
            // picker — and taking the mode back would take that away.
            return;
        }
        let standing =
            self.selections().is_some_and(|s| s.all().iter().any(|sel| !sel.is_collapsed()));
        if standing {
            self.session.mode = Mode::Visual(shape);
        }
    }

    /// `:set <option> <value>`, or `:set <option>=<value>` — vim's spelling,
    /// which the fingers type without asking. Bare `:set <option>` reports.
    ///
    /// The names and their meanings live in [`Options`], not here, so `:set`
    /// and `config.toml` cannot disagree about what an option is or what it
    /// accepts.
    /// `:whitespace` — the same layer `:set whitespace` writes to.
    ///
    /// It goes through `set_option` rather than reaching for the field, so the
    /// override is remembered and re-applied over the next file's type and
    /// project the way every other `:set` is. A toggle reads the value in force
    /// *where the cursor is*, not the session's: that is the one you can see,
    /// and flipping the one behind it would turn the mark off in a window that
    /// never had it on.
    /// `:yname {register}` — stores the capture the `"n` prompt is holding.
    ///
    /// Renaming is re-yanking: an existing name is simply replaced, because a
    /// prompt that stops to ask "are you sure" about a register is slower
    /// than yanking again.
    fn name_pending(&mut self, name: &str) {
        match self.session.pending_named.take() {
            Some(entry) => {
                self.session.registers.set_named(name, entry);
                self.session.status = format!("named \"{name}\"");
            }
            None => self.session.status = "nothing to name — `\"n` captures first".into(),
        }
    }

    fn set_whitespace(&mut self, on: Option<bool>) {
        let on = on.unwrap_or(!self.options().whitespace);
        self.set_option(&format!("whitespace {on}"));
        self.session.status = format!("whitespace={on}");
    }

    fn set_option(&mut self, arg: &str) {
        let (name, value) = match arg.split_once(['=', ' ']) {
            Some((name, value)) => (name.trim(), value.trim()),
            None => (arg.trim(), ""),
        };

        if name.is_empty() {
            self.session.status = "set what?".into();
            return;
        }

        if value.is_empty() {
            // `syntax` reports what is *in force*, not the override, because
            // the override is empty in the normal case and "syntax=" answers
            // the question nobody asked. The buffer is in reach here, which is
            // the only place it is.
            if name == "syntax" {
                let effective = match self.buffer() {
                    Some(buffer) => wanted_syntax(buffer, self.options()).unwrap_or("none"),
                    None => "none",
                };
                self.session.status = format!("syntax={effective}");
                return;
            }
            self.session.status = match self.session.options.get(name) {
                Some(OptionValue::Int(n)) => format!("{name}={n}"),
                Some(OptionValue::Bool(on)) => format!("{name}={on}"),
                Some(OptionValue::Str(s)) => format!("{name}={s}"),
                // `get` never yields `Other`: no option stores one. Reported
                // as unknown rather than asserted away, because a future
                // option whose `get` arm is wrong should produce a message,
                // not take the editor down mid-session.
                Some(OptionValue::Other) | None => format!("unknown option: {name}"),
            };
            return;
        }

        // The typed value `Options::set` wants. A bare word that is neither a
        // number nor a bool still goes through, so the option itself gets to
        // say what it wanted rather than this function guessing. Case-
        // sensitive on purpose: TOML booleans are lowercase-only, and
        // `:set hlsearch true` should accept exactly what `hlsearch = true`
        // in config.toml accepts, no more.
        let parsed = match value.parse::<i64>() {
            Ok(n) => OptionValue::Int(n),
            Err(_) => match value {
                "true" => OptionValue::Bool(true),
                "false" => OptionValue::Bool(false),
                // A bare word is a string now that one option takes one.
                // An option that wants something else still gets to say so.
                other => OptionValue::Str(other.to_string()),
            },
        };

        let was = self.session.options.active_theme(self.remote).to_string();
        let was_italics = self.session.options.italics;
        if let Err(message) = self.session.options.set(name, parsed.clone()) {
            // A real option given a bad value gets the value echoed — you
            // want to see what you fat-fingered. An unknown option does not:
            // its message already names the thing that was wrong.
            self.session.status = match self.session.options.get(name) {
                Some(_) => format!("{message}: {value}"),
                None => message,
            };
            return;
        }

        // Remembered as a layer, not only as a value: it has to be re-applied
        // over whatever the next file's type and project ask for, or `:set`
        // would be the one layer a `.editorconfig` could silently overrule.
        self.session.overrides.set(name, parsed);
        self.resolve_options();

        // A name is not a palette. `:set theme ansi` that left `self.theme`
        // alone would report success and change nothing on screen — and
        // `:set italics` is the same failure, since the palette is where the
        // slants were dropped.
        if self.session.options.active_theme(self.remote) != was
            || self.session.options.italics != was_italics
        {
            let source = self.config_source.take();
            let problems = self.resolve_theme(source.as_deref());
            self.config_source = source;
            self.session.status = match problems.first() {
                Some(problem) => problem.message.clone(),
                // Echo back the theme actually in force rather than the name
                // that was typed — over SSH `:set theme` moves the one that is
                // not live, and saying so is the point. `:set italics` echoes
                // itself, because the theme did not change.
                None if name == "italics" => {
                    format!("italics={}", self.session.options.italics)
                }
                None => format!("{name}={}", self.session.options.active_theme(self.remote)),
            };
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
        // The hover goes out when you do the next thing — the flash's rule,
        // and the same one keystroke.
        self.session.hover = None;
        // While the menu is up, a handful of insert keys mean the menu
        // rather than the buffer — decided here, so `Input` never learns
        // whether one is open.
        if self.intercept_completion(&cmd.action) {
            return;
        }
        let action = cmd.action.clone();
        match cmd.action {
            Action::Buffer(buffer_cmd) => self.run_buffer_cmd(buffer_cmd),
            Action::Window(window_cmd) => self.run_window_cmd(window_cmd),
            Action::Tree(tree_cmd) => self.run_tree_cmd(tree_cmd),
            Action::Results(results_cmd) => {
                self.run_results_cmd(results_cmd, cmd.count.max(1));
            }
            _ => {
                // A picture reads a handful of these as pixels and swallows
                // the mode-entering ones; the rest of what is left needs the
                // rope, and neither a tree nor an image window has one.
                if self.run_image_action(&cmd) {
                } else if !self.run_session_action(&cmd.action)
                    && let Some(mut view) = self.focused()
                {
                    view.apply(cmd);
                }
            }
        }
        // After, so the trigger logic reads the buffer the command left
        // behind — the char is in, the cursor has moved.
        self.sync_completion(&action);
        self.sync_signature(&action);

        // A capture waiting on its name gets the prompt now, after the whole
        // command has settled its modes — a visual operator ends visual mode
        // *after* capturing, and a prompt opened inside would not survive it.
        if self.session.pending_named.is_some() && !matches!(self.session.mode, Mode::Command(_)) {
            self.session.status.clear();
            self.session.mode = Mode::Command("yname ".into());
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
    /// A bracketed paste from the terminal: text arriving all at once rather
    /// than as the keystrokes it looks like.
    ///
    /// One action however long it is, so it costs one undo entry and one
    /// reparse instead of one per character — which is what made a paste
    /// appear a letter at a time.
    ///
    /// A paste is *typed text*, so it goes where typed text goes: the buffer in
    /// insert or replace, the `:` line, the search line. In the modes that take
    /// no text it is refused rather than invented into a put — `"+p` is the
    /// command for that, and it says what it does. See
    /// `docs/specs/clipboard.md`.
    pub fn paste_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        let action = match self.session.mode {
            Mode::Insert | Mode::Replace => Action::InsertText(text),
            Mode::Command(_) => {
                for c in text.chars().filter(|c| *c != '\n' && *c != '\r') {
                    self.apply(Command { count: 1, action: Action::CommandChar(c) });
                }
                return;
            }
            Mode::Search { .. } => {
                for c in text.chars().filter(|c| *c != '\n' && *c != '\r') {
                    self.apply(Command { count: 1, action: Action::SearchChar(c) });
                }
                return;
            }
            _ => {
                self.session.status = "paste into insert mode, or use \"+p".into();
                return;
            }
        };
        self.apply(Command { count: 1, action });
    }

    pub fn settle(&mut self) {
        self.attach_lsp();
        self.drain_edits();
        // After the drain: a completion or signature ask must trail the
        // `didChange` that carries the char that triggered it, or the server
        // answers about yesterday's text.
        self.flush_completion();
        self.flush_signature();
        self.flush_saves();
        self.pump_lsp();
        // The pump can itself edit — a `:format` answer — so the drain runs
        // once more: the parse tree and the servers see those edits before
        // the frame that shows them, not one keystroke later.
        self.drain_edits();
        // Last, so the settle that sees a handshake complete is the settle
        // that opens the documents waiting on it.
        self.open_lsp_docs();
    }

    /// The drain itself: every buffer's `pending_edits`, fed to tree-sitter,
    /// to LSP `didChange`, to the stored diagnostics, and to the selections
    /// of every window not responsible for them.
    fn drain_edits(&mut self) {
        let focus = self.focus;
        for entry in &mut self.buffers {
            let edits = std::mem::take(&mut entry.buffer.pending_edits);
            if edits.is_empty() {
                continue;
            }
            if let Some(syntax) = &mut entry.syntax {
                syntax.update(entry.buffer.rope(), &edits);
            }

            // The drain's git consumer: the signs follow the text within the
            // keystroke, exactly as the parse tree does.
            if let Some(git) = &mut entry.git
                && git.seen != entry.buffer.edits()
            {
                git.diff = crate::git::diff(&git.baseline, &entry.buffer.rope().to_string());
                git.seen = entry.buffer.edits();
            }

            // The drain's second consumer, promised since the field was
            // designed: the server's copy follows the text, and the stored
            // diagnostics follow it exactly as the selections below do.
            if let lsp::Attach::Doc(doc) = &mut entry.lsp {
                for diag in &mut doc.diagnostics {
                    diag.start = edits.iter().fold(diag.start, |at, e| e.map(at));
                    diag.end = edits.iter().fold(diag.end, |at, e| e.map(at));
                }
                self.lsp.change(doc, entry.buffer.rope(), &edits);
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

    /// Resolves LSP attachment for any buffer that has none — lazily, here,
    /// because settle runs after every event: a file opened any way at all is
    /// looked at one event later at most, and the answer is cached on the
    /// entry until the config moves. See `docs/specs/lsp.md`.
    fn attach_lsp(&mut self) {
        let epoch = self.config_epoch;
        for i in 0..self.buffers.len() {
            let entry = &self.buffers[i];
            let stale = match &entry.lsp {
                lsp::Attach::Unresolved => true,
                lsp::Attach::No { epoch: at, .. } => *at != epoch,
                lsp::Attach::Doc(_) => false,
            };
            if !stale {
                continue;
            }
            let no = |reason: &str| lsp::Attach::No { epoch, reason: reason.into() };
            let resolved = if !self.config.lsp.enabled {
                no("off (`enabled = false` in [lsp])")
            } else if let Some(path) = entry.buffer.path.clone() {
                match entry.filetype {
                    Some(filetype) => {
                        match self.lsp.attach(entry.id, &path, filetype, &self.config.lsp.servers) {
                            Ok(doc) => lsp::Attach::Doc(doc),
                            Err(reason) => lsp::Attach::No { epoch, reason },
                        }
                    }
                    None => no("no filetype for this buffer"),
                }
            } else {
                no("no file behind this buffer")
            };
            self.buffers[i].lsp = resolved;
        }
    }

    /// `didSave` for every write since the last settle.
    fn flush_saves(&mut self) {
        let saved = std::mem::take(&mut self.session.pending_saves);
        // A write is one of the moments the file's standing with the index
        // can have moved — `git add -p` between saves, most commonly.
        for &id in &saved {
            self.refresh_git(id);
        }
        for id in saved {
            let Some(entry) = self.buffers.iter_mut().find(|b| b.id == id) else { continue };
            let lsp::Attach::Doc(doc) = &mut entry.lsp else { continue };
            let current = entry
                .buffer
                .path
                .as_deref()
                .and_then(|p| lsp::pos::canonical(p).ok())
                .map(|p| lsp::pos::uri_of(&p));
            if current.as_deref() == Some(doc.uri.as_str()) {
                self.lsp.saved(doc, entry.buffer.rope());
            } else {
                // `:w other.rs` moved the file under the document: the server
                // is told the old one closed, and the new path attaches fresh
                // on the next settle.
                self.lsp.close(doc);
                entry.lsp = lsp::Attach::Unresolved;
            }
        }
    }

    /// Applies everything the servers sent since the last settle.
    fn pump_lsp(&mut self) {
        for (from, msg) in self.lsp.drain() {
            match self.lsp.accept(from, msg) {
                Some(lsp::Effect::Status(status)) => self.session.status = status,
                Some(lsp::Effect::Diagnostics { buffer, version, diagnostics, encoding }) => {
                    self.store_diagnostics(buffer, version, diagnostics, encoding)
                }
                Some(lsp::Effect::Goto { kind, window, targets, encoding }) => {
                    self.apply_goto(kind, window, targets, encoding)
                }
                Some(lsp::Effect::References { symbol, root, targets, encoding }) => {
                    self.apply_references(symbol, root, targets, encoding)
                }
                Some(lsp::Effect::Formatting { buffer, version, edits, encoding }) => {
                    self.apply_formatting(buffer, version, edits, encoding)
                }
                Some(lsp::Effect::Hover { window, anchor, markdown }) => {
                    self.apply_hover(window, anchor, markdown)
                }
                Some(lsp::Effect::Completion {
                    buffer,
                    request,
                    manual,
                    incomplete,
                    items,
                    encoding,
                }) => self.apply_completion(buffer, request, manual, incomplete, items, encoding),
                Some(lsp::Effect::Signature { request, help }) => {
                    self.apply_signature(request, help)
                }
                None => {}
            }
        }
    }

    /// `didOpen` for any attached document whose server has finished its
    /// handshake — which is also what lets the open carry whatever was typed
    /// while the server was starting. See `Registry::try_open`.
    fn open_lsp_docs(&mut self) {
        for entry in &mut self.buffers {
            if let lsp::Attach::Doc(doc) = &mut entry.lsp
                && !doc.opened
                && let Some(filetype) = entry.filetype
            {
                self.lsp.try_open(doc, filetype, entry.buffer.rope());
            }
        }
    }

    /// One `publishDiagnostics`, converted to char offsets and stored.
    fn store_diagnostics(
        &mut self,
        buffer: BufferId,
        version: Option<i32>,
        diagnostics: Vec<lsp::types::Diagnostic>,
        encoding: lsp::pos::Encoding,
    ) {
        let Some(entry) = self.buffers.iter_mut().find(|b| b.id == buffer) else { return };
        let lsp::Attach::Doc(doc) = &mut entry.lsp else { return };
        // Tagged with a version other than the current one: these describe
        // text that no longer exists, and their successor is already being
        // computed. Untagged ones are taken at their word.
        if version.is_some_and(|v| v != doc.version) {
            return;
        }
        let rope = entry.buffer.rope();
        doc.diagnostics = diagnostics
            .into_iter()
            .map(|d| lsp::Diag {
                start: lsp::pos::char_of(rope, d.range.start, encoding),
                end: lsp::pos::char_of(rope, d.range.end, encoding),
                severity: lsp::Severity::from_wire(d.severity),
                message: d.message,
                source: d.source,
            })
            .collect();
        // In buffer order, which is the order everything that will draw or
        // walk them wants.
        doc.diagnostics.sort_by_key(|d| (d.start, d.end));
    }

    /// The focused buffer's live document, for a request about to be sent —
    /// or the status-line reason there is nothing to ask.
    ///
    /// Everything owned, because the caller's next move is a `&mut` call on
    /// the registry the borrows would otherwise pin.
    fn lsp_target(&self) -> Result<(BufferId, lsp::Doc, lsp::client::Caps), String> {
        let Some(id) = self.window().buffer() else {
            return Err("no buffer in this window".into());
        };
        match &self.entry(id).lsp {
            lsp::Attach::Doc(doc) if doc.opened => {
                let caps = self
                    .lsp
                    .instance(doc.server)
                    .map(|c| c.caps)
                    .ok_or("lsp: instance gone — :lsp restart")?;
                Ok((id, doc.clone(), caps))
            }
            lsp::Attach::Doc(_) => Err("lsp: server is still starting".into()),
            lsp::Attach::Unresolved => Err("lsp: not attached".into()),
            lsp::Attach::No { reason, .. } => Err(format!("lsp: {reason}")),
        }
    }

    /// The wire position of the focused cursor, in the encoding the server
    /// was granted.
    fn lsp_position(&self, id: BufferId, server: lsp::ServerId) -> Option<lsp::types::Position> {
        let encoding = self.lsp.instance(server)?.encoding;
        let at = self.window().text()?.selections.cursor().at;
        Some(lsp::pos::position_of(self.entry(id).buffer.rope(), at, encoding))
    }

    /// `:definition` — `gd` — and `:decl`, `:impl`: one goto, three kinds.
    /// Files the request; the answer jumps this window when it lands,
    /// through the pump.
    fn lsp_goto(&mut self, kind: lsp::Goto) {
        let sent = self.lsp_target().and_then(|(id, doc, caps)| {
            if !caps.offers(kind) {
                return Err(format!("{}: this server does not offer it", kind.noun()));
            }
            let position = self.lsp_position(id, doc.server).ok_or("no cursor here")?;
            self.lsp.goto(kind, &doc, position, self.focus)
        });
        if let Err(status) = sent {
            self.session.status = status;
        }
    }

    /// `:peek` — the definition beside you: a vertical split, the same
    /// `:definition` the `gd` key runs, focus on the answer. See
    /// `docs/specs/lsp-requests.md`.
    fn peek_definition(&mut self) {
        // The server is asked about *before* the split: a `:peek` with
        // nothing to show must not leave an empty split behind.
        match self.lsp_target() {
            Ok((_, _, caps)) if caps.definition => {}
            Ok(_) => {
                self.session.status = "definition: this server does not offer it".into();
                return;
            }
            Err(status) => {
                self.session.status = status;
                return;
            }
        }
        let before = self.window_ids().len();
        self.run_window_cmd(WindowCmd::Split { dir: Dir::Vertical, path: None });
        if self.window_ids().len() == before {
            // No room; the split said so.
            return;
        }
        // Focus is the new window now, and the request carries it — the
        // answer lands in the split, not where you were reading.
        self.lsp_goto(lsp::Goto::Definition);
    }

    /// `]]` / `[[` — the next or previous boundary, starts and ends both
    /// stops. At the last one it stays put rather than wrapping: a boundary
    /// walk is local, and teleporting to the top of the file is not walking.
    /// See `docs/specs/boundaries.md`.
    fn boundary_jump(&mut self, forward: bool) {
        let Some(buffer) = self.buffer() else { return };
        let Some(syntax) = self.syntax() else {
            self.session.status = "no syntax tree here".into();
            return;
        };
        let rope = buffer.rope();
        let stops = boundary_positions(syntax, rope);
        let Some(cursor) = self.cursor() else { return };
        let target = match forward {
            true => stops.iter().find(|&&at| at > cursor.at),
            false => stops.iter().rev().find(|&&at| at < cursor.at),
        };
        let Some(&at) = target else { return };
        self.set_cursor(Cursor::at(at));
    }

    /// `:ts` — the boundaries on show, or put away. See
    /// `docs/specs/boundaries.md`.
    fn toggle_ts_marks(&mut self) {
        if !self.session.ts_marks && self.syntax().is_none() {
            self.session.status = "no syntax tree here".into();
            return;
        }
        self.session.ts_marks = !self.session.ts_marks;
        self.session.status =
            if self.session.ts_marks { "boundaries on" } else { "boundaries off" }.into();
    }

    /// `:zen` — the chrome off, and back. The frontend reads the flag; what
    /// stops being drawn was always the frontend's. See `docs/specs/zen.md`.
    fn toggle_zen(&mut self) {
        self.session.zen = !self.session.zen;
        self.session.status = if self.session.zen { "zen on" } else { "zen off" }.into();
    }

    /// `:tssplit` — the bracketed list around the cursor, one element per
    /// line, reindented by the machinery `=` uses. See
    /// `docs/specs/splitjoin.md`.
    fn ts_split(&mut self) {
        self.in_view(|view| {
            let Some(syntax) = view.syntax.as_ref() else {
                view.session.status = "no syntax tree here".into();
                return;
            };
            let rope = view.buffer.rope();
            let cursor = view.selections.cursor().at.min(rope.len_chars());
            let Some(list) = syntax.list_at(rope.char_to_byte(cursor)) else {
                view.session.status = "no brackets around the cursor".into();
                return;
            };
            // Everything in chars, before the first edit moves anything.
            let open = rope.byte_to_char(list.open_end);
            let commas: Vec<usize> = list.commas.iter().map(|&b| rope.byte_to_char(b)).collect();
            let close = rope.byte_to_char(list.close_start);

            // A break point is skipped when a newline is already doing its
            // job — nothing but blanks between it and the break either way —
            // so a half-split list splits the rest of the way rather than
            // gaining blank lines.
            let blank_to_eol = |at: usize| {
                rope.chars_at(at).take_while(|&c| c != '\n').all(|c| matches!(c, ' ' | '\t' | '\r'))
            };
            let blank_from_bol = |at: usize| {
                let start = rope.line_to_char(rope.char_to_line(at));
                rope.slice(start..at).chars().all(|c| matches!(c, ' ' | '\t'))
            };
            let mut points: Vec<usize> = Vec::new();
            if !blank_to_eol(open) {
                points.push(open);
            }
            points.extend(commas.iter().copied().filter(|&at| !blank_to_eol(at)));
            if !blank_from_bol(close) {
                points.push(close);
            }
            if points.is_empty() {
                view.session.status = "already split".into();
                return;
            }

            let before = view.selections.as_pairs();
            let applied_from = view.buffer.pending_edits.len();
            // Back to front, so a break cannot move the ones still to come.
            for &at in points.iter().rev() {
                view.buffer.insert_str(Cursor::at(at), "\n");
            }
            // Every insertion sat at or before the closer, so its new home is
            // known without asking. The opening line keeps its own indent —
            // it is the anchor the rows under it are computed from.
            let first = view.buffer.row_at(Cursor::at(open.saturating_sub(1)));
            let last = view.buffer.row_at(Cursor::at(close + points.len()));
            let indent = view.options.indent();
            view.buffer.reindent_rows(first + 1, last, &indent);

            // The focused window's selections are this command's to carry —
            // `settle` deliberately skips them.
            let applied = view.buffer.pending_edits[applied_from..].to_vec();
            let across = move |at: usize| applied.iter().fold(at, |at, e| e.map(at));
            let mapped: Vec<Selection> = view
                .selections
                .all()
                .iter()
                .map(|s| Selection {
                    anchor: Cursor::at(across(s.anchor.at)),
                    head: Cursor::at(across(s.head.at)),
                })
                .collect();
            view.selections.set(mapped);
            view.buffer.commit_undo(before, view.selections.as_pairs());
        });
    }

    /// `:tsjoin` — the same list back onto one line: each line trimmed and
    /// joined with a space, nothing against the brackets, and a trailing
    /// comma left pressed against the closer comes off. See
    /// `docs/specs/splitjoin.md`.
    fn ts_join(&mut self) {
        self.in_view(|view| {
            let Some(syntax) = view.syntax.as_ref() else {
                view.session.status = "no syntax tree here".into();
                return;
            };
            let rope = view.buffer.rope();
            let cursor = view.selections.cursor().at.min(rope.len_chars());
            let Some(list) = syntax.list_at(rope.char_to_byte(cursor)) else {
                view.session.status = "no brackets around the cursor".into();
                return;
            };
            let open = rope.byte_to_char(list.open_end);
            let close = rope.byte_to_char(list.close_start);
            let inner = rope.slice(open..close).to_string();
            if !inner.contains('\n') {
                view.session.status = "already one line".into();
                return;
            }
            let mut joined = inner
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if joined.ends_with(',') {
                joined.pop();
            }

            let before = view.selections.as_pairs();
            let applied_from = view.buffer.pending_edits.len();
            view.buffer.replace_range(open, close, &joined);
            let applied = view.buffer.pending_edits[applied_from..].to_vec();
            let across = move |at: usize| applied.iter().fold(at, |at, e| e.map(at));
            let mapped: Vec<Selection> = view
                .selections
                .all()
                .iter()
                .map(|s| Selection {
                    anchor: Cursor::at(across(s.anchor.at)),
                    head: Cursor::at(across(s.head.at)),
                })
                .collect();
            view.selections.set(mapped);
            view.buffer.commit_undo(before, view.selections.as_pairs());
        });
    }

    /// `:references` — `gr`. The answer becomes a `Results` pane.
    fn lsp_references(&mut self) {
        let sent = self.lsp_target().and_then(|(id, doc, caps)| {
            if !caps.references {
                return Err("references: this server does not offer it".into());
            }
            let position = self.lsp_position(id, doc.server).ok_or("no cursor here")?;
            // The symbol travels with the intent: it is the pane's title and
            // its query, and by the time the answer lands the cursor may be
            // on a different word entirely.
            let symbol = {
                let buffer = &self.entry(id).buffer;
                let cursor =
                    self.window().text().expect("lsp_target found a buffer").selections.cursor();
                match buffer.word_at(cursor) {
                    Some((start, end)) => buffer.rope().slice(start..end).to_string(),
                    None => return Err("no word under the cursor".into()),
                }
            };
            self.lsp.references(&doc, position, symbol)
        });
        if let Err(status) = sent {
            self.session.status = status;
        }
    }

    /// `:format` — the whole file, by the server, as one undo step.
    fn lsp_format(&mut self) {
        let sent = self.lsp_target().and_then(|(id, doc, caps)| {
            if !caps.formatting {
                return Err("format: this server does not offer it".into());
            }
            let options = &self.entry(id).options;
            self.lsp.formatting(&doc, id, options.tab_width, options.expandtab)
        });
        if let Err(status) = sent {
            self.session.status = status;
        }
    }

    /// A goto answer: open the first target where the request was made.
    fn apply_goto(
        &mut self,
        kind: lsp::Goto,
        window: WindowId,
        targets: Vec<(PathBuf, lsp::types::Range)>,
        encoding: lsp::pos::Encoding,
    ) {
        let Some((path, range)) = targets.first().cloned() else {
            self.session.status = format!("no {} found", kind.noun());
            return;
        };
        // The window that asked may have closed while the server thought.
        if self.window_of(window).is_none() {
            return;
        }
        let id = match self.open_path(&path.to_string_lossy()) {
            Ok(id) => id,
            Err(e) => {
                self.session.status = format!("error: {e:#}");
                return;
            }
        };
        self.show(window, id);
        let at = lsp::pos::char_of(self.entry(id).buffer.rope(), range.start, encoding);
        if let Some(text) = self.window_mut_of(window).and_then(Window::text_mut) {
            text.selections = Selections::from_pairs(vec![(at, at)]);
        }
        if targets.len() > 1 {
            self.session.status =
                format!("went to the first of {} {}s", targets.len(), kind.noun());
        }
    }

    /// A references answer, as the pane `find-in-files.md` promised it.
    fn apply_references(
        &mut self,
        symbol: String,
        root: PathBuf,
        targets: Vec<(PathBuf, lsp::types::Range)>,
        encoding: lsp::pos::Encoding,
    ) {
        if targets.is_empty() {
            self.session.status = format!("no references to {symbol}");
            return;
        }

        let mut unreadable = 0;
        let mut matches: Vec<crate::find_in_files::Match> = Vec::new();
        for (path, range) in &targets {
            let row = range.start.line as usize;
            let Some(text) = self.line_text(path, row) else {
                unreadable += 1;
                continue;
            };
            let col = lsp::pos::col_to_char(&text, range.start.character, encoding);
            // A range that runs past its line — a multi-line reference —
            // counts to the line's end; the row is still the answer.
            let len = match range.end.line as usize == row {
                true => lsp::pos::col_to_char(&text, range.end.character, encoding),
                false => text.chars().count(),
            }
            .saturating_sub(col)
            .max(1);
            let rel = path.strip_prefix(&root).unwrap_or(path).to_path_buf();
            matches.push(crate::find_in_files::Match { path: rel, line: row + 1, col, len, text });
        }
        // Servers answer in whatever order indexing found them; the pane
        // groups by file, so the matches have to arrive that way.
        matches.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));

        if matches.is_empty() {
            self.session.status = format!("references to {symbol}: none readable");
            return;
        }

        let files = {
            let mut n = 0;
            let mut last: Option<&std::path::PathBuf> = None;
            for m in &matches {
                if last != Some(&m.path) {
                    n += 1;
                    last = Some(&m.path);
                }
            }
            n
        };
        let mut report = format!(
            "{} reference{} in {files} file{}",
            matches.len(),
            if matches.len() == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" },
        );
        if unreadable > 0 {
            report.push_str(&format!(", {unreadable} unreadable"));
        }

        // The query is the symbol, which is what makes `:replace` over this
        // pane a rename spelled with two commands bi already has.
        let query = crate::find_in_files::Query {
            pattern: symbol.clone(),
            regex: false,
            gitignore: self.options().gitignore,
            ..Default::default()
        };
        let results =
            crate::results::Results::new(format!("references: {symbol}"), query, root, matches);
        self.show_results(results);
        self.session.status = report;
    }

    /// `:diags` — every open buffer's stored diagnostics, as the pane
    /// `find-in-files.md` promised them: Enter jumps to the diagnosed span,
    /// and the first line of the message rides after the text. Open buffers
    /// only, because that is what the store holds. See
    /// `docs/specs/diagnostics.md`.
    fn show_diagnostics(&mut self) {
        let root = self.tree_root(self.buffer().and_then(|b| b.path.as_deref()));
        let mut matches: Vec<crate::find_in_files::Match> = Vec::new();
        let mut files = 0;
        for entry in &self.buffers {
            let lsp::Attach::Doc(doc) = &entry.lsp else { continue };
            let Some(path) = &entry.buffer.path else { continue };
            if doc.diagnostics.is_empty() {
                continue;
            }
            files += 1;
            let rel = path.strip_prefix(&root).unwrap_or(path).to_path_buf();
            let mut sorted: Vec<&lsp::Diag> = doc.diagnostics.iter().collect();
            // Worst first within a file, reading order within a severity —
            // the list is for jumping, and the error outranks the hint.
            sorted.sort_by_key(|d| (d.severity, d.start));
            for d in sorted {
                let row = entry.buffer.row_at(Cursor::at(d.start));
                let start = entry.buffer.rope().line_to_char(row);
                let line = entry.buffer.rope().line(row).to_string();
                let line = line.trim_end_matches(['\n', '\r']);
                let col = d.start - start;
                // A range that runs past its line counts to the line's end;
                // the row is still the answer — `apply_references`' rule.
                let len = match entry.buffer.row_at(Cursor::at(d.end)) == row {
                    true => d.end - d.start,
                    false => line.chars().count().saturating_sub(col),
                }
                .max(1);
                let sev = match d.severity {
                    lsp::Severity::Error => "E",
                    lsp::Severity::Warning => "W",
                    lsp::Severity::Info => "I",
                    lsp::Severity::Hint => "H",
                };
                let message = d.message.lines().next().unwrap_or("");
                matches.push(crate::find_in_files::Match {
                    path: rel.clone(),
                    line: row + 1,
                    col,
                    len,
                    text: format!("{line}  ▸ {sev}: {message}"),
                });
            }
        }
        if matches.is_empty() {
            self.session.status = "no diagnostics".into();
            return;
        }
        let report = format!(
            "{} diagnostic{} in {files} file{}",
            matches.len(),
            if matches.len() == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" },
        );
        let query = crate::find_in_files::Query {
            pattern: String::new(),
            regex: false,
            gitignore: self.options().gitignore,
            ..Default::default()
        };
        self.show_results(crate::results::Results::new("diagnostics".into(), query, root, matches));
        self.session.status = report;
    }

    /// One line of a file, from the open buffer when there is one — unsaved
    /// edits included — and from the disk otherwise.
    fn line_text(&self, path: &std::path::Path, row: usize) -> Option<String> {
        let wanted = lsp::pos::canonical(path).ok()?;
        for entry in &self.buffers {
            let Some(p) = &entry.buffer.path else { continue };
            if lsp::pos::canonical(p).ok().as_deref() == Some(&wanted) {
                return (row < entry.buffer.line_count()).then(|| entry.buffer.line(row));
            }
        }
        let text = std::fs::read_to_string(&wanted).ok()?;
        text.lines().nth(row).map(str::to_string)
    }

    /// A formatting answer: the server's edits, applied as one undo step.
    fn apply_formatting(
        &mut self,
        id: BufferId,
        version: i32,
        edits: Vec<lsp::types::TextEdit>,
        encoding: lsp::pos::Encoding,
    ) {
        if edits.is_empty() {
            self.session.status = "already formatted".into();
            return;
        }
        let focus = self.focus;
        let focused_here = self.window_of(focus).and_then(Window::buffer) == Some(id);
        // Undo lands you where you were when you asked — the live selections
        // when the buffer is on screen, its parked ones when it is not.
        let current = self
            .window_of(focus)
            .and_then(Window::text)
            .filter(|_| focused_here)
            .map(|t| t.selections.as_pairs());
        let Some(entry) = self.buffers.iter_mut().find(|b| b.id == id) else { return };

        // The version gate: a format computed against text that no longer
        // exists must not touch the text that replaced it.
        match &entry.lsp {
            lsp::Attach::Doc(doc) if doc.version == version => {}
            _ => {
                self.session.status = "text changed under :format — run it again".into();
                return;
            }
        }

        // Converted against the current rope — which the version gate just
        // proved is the text the edits were computed for — then applied
        // bottom-up so an earlier edit cannot move a later one.
        let rope = entry.buffer.rope();
        let mut spans: Vec<(usize, usize, String)> = edits
            .iter()
            .map(|e| {
                (
                    lsp::pos::char_of(rope, e.range.start, encoding),
                    lsp::pos::char_of(rope, e.range.end, encoding),
                    e.new_text.clone(),
                )
            })
            .collect();
        spans.sort_by(|a, b| b.0.cmp(&a.0));

        let before = current.unwrap_or_else(|| entry.last.clone());
        let applied_from = entry.buffer.pending_edits.len();
        for (start, end, text) in &spans {
            entry.buffer.replace_range(*start, *end, text);
        }
        let across = {
            let applied = entry.buffer.pending_edits[applied_from..].to_vec();
            move |at: usize| applied.iter().fold(at, |at, e| e.map(at))
        };

        // The focused window is the one `settle` deliberately skips, so its
        // selections are this function's to carry.
        let after: crate::history::Cursors =
            before.iter().map(|&(a, h)| (across(a), across(h))).collect();
        entry.buffer.commit_undo(before, after);
        if focused_here && let Some(text) = self.window_mut_of(focus).and_then(Window::text_mut) {
            let mapped: Vec<Selection> = text
                .selections
                .all()
                .iter()
                .map(|s| Selection {
                    anchor: Cursor::at(across(s.anchor.at)),
                    head: Cursor::at(across(s.head.at)),
                })
                .collect();
            text.selections.set(mapped);
        }
        self.session.status =
            format!("formatted — {} edit{}", spans.len(), if spans.len() == 1 { "" } else { "s" });
    }

    /// `:dnext` / `:dprev` — `]d` / `[d`. Wraps, and puts the message on the
    /// status line, which is also where a message too long for its EOL tail
    /// can be read whole.
    fn diagnostic_jump(&mut self, forward: bool) {
        let Some(id) = self.window().buffer() else {
            self.session.status = "no buffer in this window".into();
            return;
        };
        let diagnostics = self.diagnostics(id);
        if diagnostics.is_empty() {
            self.session.status = "no diagnostics here".into();
            return;
        }
        let Some(at) = self.cursor().map(|c| c.at) else { return };
        let index = match forward {
            true => diagnostics.iter().position(|d| d.start > at).unwrap_or(0),
            false => match diagnostics.iter().rposition(|d| d.start < at) {
                Some(i) => i,
                None => diagnostics.len() - 1,
            },
        };
        let d = &diagnostics[index];
        let (start, message) = (d.start, d.message.lines().next().unwrap_or("").to_string());
        self.session.status = format!("[{}/{}] {message}", index + 1, diagnostics.len());
        self.set_cursor(Cursor::at(start));
    }

    /// `:hover` — `K`. The answer floats at the char it was asked about.
    fn lsp_hover(&mut self) {
        let sent = self.lsp_target().and_then(|(id, doc, caps)| {
            if !caps.hover {
                return Err("hover: this server does not offer it".into());
            }
            let position = self.lsp_position(id, doc.server).ok_or("no cursor here")?;
            let anchor = self.cursor().map(|c| c.at).ok_or("no cursor here")?;
            self.lsp.hover(&doc, position, self.focus, anchor)
        });
        if let Err(status) = sent {
            self.session.status = status;
        }
    }

    /// A hover answer: processed to lines and parked on the session for the
    /// frontend to float. Cleared by the next command, like the flash.
    fn apply_hover(&mut self, window: WindowId, anchor: usize, markdown: Option<String>) {
        let Some(markdown) = markdown else {
            self.session.status = "no hover info here".into();
            return;
        };
        if self.window_of(window).is_none() {
            return;
        }
        let language = self.window_of(window).and_then(Window::buffer).and_then(|id| {
            self.buffers.iter().find(|b| b.id == id).and_then(|entry| entry.filetype)
        });
        let lines = hover_lines(&markdown);
        if lines.is_empty() {
            self.session.status = "no hover info here".into();
            return;
        }
        self.session.hover = Some(Hover { window, anchor, lines, language });
    }

    // ---- completion ---------------------------------------------------------

    /// The insert keys that mean the menu while one is up — and `Ctrl-N` as
    /// the manual summons while none is. `true` means the key is spent.
    fn intercept_completion(&mut self, action: &Action) -> bool {
        if self.session.completion.is_some() {
            match action {
                Action::CompleteNext => {
                    if let Some(menu) = &mut self.session.completion {
                        menu.shift(true);
                    }
                }
                Action::CompletePrev | Action::InsertIndent { right: false } => {
                    if let Some(menu) = &mut self.session.completion {
                        menu.shift(false);
                    }
                }
                Action::InsertIndent { right: true } | Action::InsertNewline => {
                    self.complete_accept();
                }
                // Close and *stay in insert*: the menu was the thing being
                // dismissed, not the typing.
                Action::EnterNormal => self.close_completion(),
                _ => return false,
            }
            return true;
        }
        match action {
            Action::CompleteNext => {
                self.complete_want = Some(CompleteWant::Manual);
                true
            }
            Action::CompletePrev => true,
            _ => false,
        }
    }

    /// After a command: what the menu does about it — narrow, close, or open.
    fn sync_completion(&mut self, action: &Action) {
        if self.session.mode != Mode::Insert {
            self.close_completion();
            return;
        }
        // An accept would need one edit per cursor; half-applying is worse
        // than none, so multi-cursor insert completes nothing for now.
        if self.selections().is_none_or(|s| s.all().len() != 1) {
            self.close_completion();
            return;
        }
        match action {
            Action::InsertChar(c) => match self.session.completion.is_some() {
                true => {
                    self.refilter_completion();
                    // The server said local narrowing cannot be trusted.
                    if self.session.completion.as_ref().is_some_and(|m| m.incomplete) {
                        self.complete_want = Some(CompleteWant::Word);
                    }
                }
                false => {
                    if is_word_char(*c) {
                        self.complete_want = Some(CompleteWant::Word);
                    } else if self.is_trigger_char(*c) {
                        self.complete_want = Some(CompleteWant::Char(*c));
                    }
                }
            },
            Action::Backspace => self.refilter_completion(),
            // A motion is leaving the word; the menu does not follow.
            Action::Move(_) => self.close_completion(),
            _ => {}
        }
    }

    fn close_completion(&mut self) {
        self.session.completion = None;
        self.complete_want = None;
    }

    /// Whether the focused buffer's server opens the menu on `c` — `.` and
    /// `::` for rust-analyzer.
    fn is_trigger_char(&self, c: char) -> bool {
        let Some(id) = self.window().buffer() else { return false };
        let lsp::Attach::Doc(doc) = &self.entry(id).lsp else { return false };
        let Some(client) = self.lsp.instance(doc.server) else { return false };
        let mut buf = [0u8; 4];
        let c = &*c.encode_utf8(&mut buf);
        client.trigger_chars.iter().any(|t| t == c)
    }

    /// Re-reads the word from the buffer and narrows the open menu — or
    /// closes it when the cursor left the word or the word left the offers.
    fn refilter_completion(&mut self) {
        let Some(start) = self.session.completion.as_ref().map(|m| m.replace.start) else {
            return;
        };
        let cursor =
            self.window_of(self.focus).and_then(Window::text).map(|t| t.selections.cursor().at);
        let shown = self.window_of(self.focus).and_then(Window::buffer);
        let (Some(at), Some(id)) = (cursor, shown) else { return self.close_completion() };
        if at < start {
            return self.close_completion();
        }
        let word: String = {
            let entry = self.buffers.iter().find(|b| b.id == id).expect("focused buffer exists");
            entry.buffer.rope().slice(start..at).to_string()
        };
        if !word.chars().all(is_word_char) {
            return self.close_completion();
        }
        let Some(menu) = &mut self.session.completion else { return };
        menu.replace.end = at;
        menu.refilter(&word);
        if menu.is_empty() {
            self.close_completion();
        }
    }

    /// After a command: whether the parameters float opens, follows, or
    /// closes. It follows by re-asking — the server already has the parser
    /// that knows which comma the cursor is behind, in every language.
    fn sync_signature(&mut self, action: &Action) {
        if self.session.mode != Mode::Insert {
            self.session.signature = None;
            self.signature_want = None;
            return;
        }
        match action {
            Action::InsertChar(c) => {
                if self.is_signature_char(*c) {
                    self.signature_want = Some(Some(*c));
                } else if self.session.signature.is_some() {
                    self.signature_want = Some(None);
                }
            }
            Action::Backspace if self.session.signature.is_some() => {
                self.signature_want = Some(None);
            }
            Action::Move(_) => {
                self.session.signature = None;
                self.signature_want = None;
            }
            _ => {}
        }
    }

    /// Whether the focused buffer's server opens the float on `c` — `(` and
    /// `,` for rust-analyzer.
    fn is_signature_char(&self, c: char) -> bool {
        let Some(id) = self.window().buffer() else { return false };
        let lsp::Attach::Doc(doc) = &self.entry(id).lsp else { return false };
        let Some(client) = self.lsp.instance(doc.server) else { return false };
        let mut buf = [0u8; 4];
        let c = &*c.encode_utf8(&mut buf);
        client.caps.signature && client.signature_chars.iter().any(|t| t == c)
    }

    /// Sends the parked signature ask — same settle timing, same reason, as
    /// `flush_completion` below.
    fn flush_signature(&mut self) {
        let Some(trigger) = self.signature_want.take() else { return };
        if self.session.mode != Mode::Insert {
            return;
        }
        let sent = self.lsp_target().and_then(|(id, doc, caps)| {
            if !caps.signature {
                return Err(String::new());
            }
            let position = self.lsp_position(id, doc.server).ok_or("")?;
            self.signature_seq += 1;
            self.lsp.signature(&doc, position, self.signature_seq, trigger)
        });
        // Automatic and cosmetic: failure is silence, never status noise.
        let _ = sent;
    }

    /// A signature answer: the float follows the cursor, or closes when the
    /// server says the call ended.
    fn apply_signature(&mut self, request: u64, help: Option<lsp::SignatureData>) {
        if request != self.signature_seq || self.session.mode != Mode::Insert {
            return;
        }
        let Some(at) = self.cursor().map(|c| c.at) else { return };
        self.session.signature = help.map(|data| Signature { anchor: at, data });
    }

    /// Sends the parked ask, from `settle`, *after* the drain — a request
    /// filed in `apply` would outrun the `didChange` carrying the char that
    /// triggered it, and the server would complete yesterday's text.
    fn flush_completion(&mut self) {
        let Some(want) = self.complete_want.take() else { return };
        if self.session.mode != Mode::Insert {
            return;
        }
        let manual = want == CompleteWant::Manual;
        let sent = self.lsp_target().and_then(|(id, doc, caps)| {
            if !caps.completion {
                return Err("completion: this server does not offer it".into());
            }
            let position = self.lsp_position(id, doc.server).ok_or("no cursor here")?;
            self.complete_seq += 1;
            let trigger = match want {
                CompleteWant::Char(c) => Some(c),
                CompleteWant::Word | CompleteWant::Manual => None,
            };
            self.lsp.completion(&doc, position, id, self.complete_seq, manual, trigger)
        });
        // A summoned failure is told; an automatic one per keystroke would
        // be status noise, so it is not.
        if let Err(status) = sent
            && manual
        {
            self.session.status = status;
        }
    }

    /// A completion answer: opens the menu, unless the world moved on.
    fn apply_completion(
        &mut self,
        buffer: BufferId,
        request: u64,
        manual: bool,
        incomplete: bool,
        items: Vec<lsp::types::CompletionItem>,
        encoding: lsp::pos::Encoding,
    ) {
        // Stale, or the moment has passed: a newer ask is in flight, insert
        // mode ended, or the cursor is in a different buffer now.
        if request != self.complete_seq
            || self.session.mode != Mode::Insert
            || self.window_of(self.focus).and_then(Window::buffer) != Some(buffer)
        {
            return;
        }
        let Some(at) = self.cursor().map(|c| c.at) else { return };
        if items.is_empty() {
            if manual {
                self.session.status = "no completions here".into();
            }
            return;
        }

        // The word start: back over identifier chars from the cursor. bi's
        // own range, recomputed again at accept — no stale server range can
        // get it wrong.
        let entry = self.buffers.iter().find(|b| b.id == buffer).expect("checked above");
        let rope = entry.buffer.rope();
        let mut start = at;
        while start > 0 && is_word_char(rope.char(start - 1)) {
            start -= 1;
        }
        let word: String = rope.slice(start..at).to_string();

        let items: Vec<crate::complete::Item> = items
            .into_iter()
            .map(|item| {
                let raw = item.new_text().to_string();
                let insert = match item.insert_text_format {
                    Some(2) => crate::complete::strip_snippet(&raw),
                    _ => raw,
                };
                crate::complete::Item {
                    filter: item.filter_text.clone().unwrap_or_else(|| item.label.clone()),
                    sort: item.sort_text.clone().unwrap_or_else(|| item.label.clone()),
                    label: item.label,
                    insert,
                    kind: item.kind,
                    detail: item.detail,
                    extra_edits: item.additional_text_edits,
                }
            })
            .collect();

        let mut menu =
            crate::complete::Completion::new(items, start..at, incomplete, request, encoding);
        menu.refilter(&word);
        match menu.is_empty() {
            true => {
                if manual {
                    self.session.status = "no completions here".into();
                }
                self.session.completion = None;
            }
            false => self.session.completion = Some(menu),
        }
    }

    /// Tab or Enter: the selected offer replaces the word, auto-imports and
    /// all, inside the still-open insert-mode undo group.
    fn complete_accept(&mut self) {
        let Some(menu) = self.session.completion.take() else { return };
        let Some(item) = menu.selected_item().cloned() else { return };
        let Some(at) = self.cursor().map(|c| c.at) else { return };
        let Some(id) = self.window_of(self.focus).and_then(Window::buffer) else { return };
        let start = menu.replace.start.min(at);

        let focus = self.focus;
        let entry = self.buffers.iter_mut().find(|b| b.id == id).expect("focused buffer exists");

        // Auto-imports first, bottom-up, with the word range mapped through
        // them — they are almost always far above the word, but "almost" is
        // not an invariant to lean on.
        let from = entry.buffer.pending_edits.len();
        let mut extra: Vec<(usize, usize, String)> = item
            .extra_edits
            .iter()
            .map(|e| {
                let rope = entry.buffer.rope();
                (
                    lsp::pos::char_of(rope, e.range.start, menu.encoding),
                    lsp::pos::char_of(rope, e.range.end, menu.encoding),
                    e.new_text.clone(),
                )
            })
            .collect();
        extra.sort_by(|a, b| b.0.cmp(&a.0));
        for (from_char, to, text) in &extra {
            entry.buffer.replace_range(*from_char, *to, text);
        }
        // Mapped with insertions-at-the-boundary shifting the word right —
        // the opposite of `Edit::map`'s cursor rule, and the right one here:
        // an import inserted exactly at the word's start goes before it.
        let applied = entry.buffer.pending_edits[from..].to_vec();
        let across = |at: usize| {
            applied.iter().fold(at, |at, e| match () {
                _ if e.old_end_char <= at => at - e.old_end_char + e.new_end_char,
                _ if e.start_char >= at => at,
                _ => e.start_char,
            })
        };
        let (start, at) = (across(start), across(at));

        entry.buffer.replace_range(start, at, &item.insert);
        let landed = start + item.insert.chars().count();
        if let Some(text) = self.window_mut_of(focus).and_then(Window::text_mut) {
            text.selections = Selections::from_pairs(vec![(landed, landed)]);
        }
        self.complete_want = None;
    }

    /// `:lsp`, `:lsp restart`, `:lsp stop` — all about the focused buffer.
    fn run_lsp(&mut self, cmd: LspCmd) {
        let Some(id) = self.window().buffer() else {
            self.session.status = "no buffer in this window".into();
            return;
        };
        match cmd {
            LspCmd::Status => self.session.status = self.lsp_status(id),
            LspCmd::Restart => self.lsp_restart(id),
            LspCmd::Stop => self.lsp_stop(id),
        }
    }

    /// One line: which server, what state, and what it is doing right now.
    fn lsp_status(&self, id: BufferId) -> String {
        let doc = match &self.entry(id).lsp {
            lsp::Attach::Unresolved => return "lsp: not looked at yet".into(),
            lsp::Attach::No { reason, .. } => return format!("lsp: {reason}"),
            lsp::Attach::Doc(doc) => doc,
        };
        let Some(client) = self.lsp.instance(doc.server) else {
            return "lsp: instance gone — :lsp restart".into();
        };

        let root = client.root.display().to_string();
        match &client.phase {
            lsp::client::Phase::Starting => format!("{}: starting · {root}", client.name),
            lsp::client::Phase::Dead { reason } => {
                // The stderr tail is the epitaph: the first question about a
                // dead server is "what did it say on the way out".
                let last = client.stderr_tail().last().cloned();
                let stderr = last.map(|l| format!(" · stderr: {l}")).unwrap_or_default();
                format!("{}: {reason} — :lsp restart{stderr}", client.name)
            }
            lsp::client::Phase::Running => {
                let n = doc.diagnostics.len();
                let mut parts = vec![
                    format!("{}: running", client.name),
                    root,
                    client.encoding.name().to_string(),
                    format!("{n} diagnostic{}", if n == 1 { "" } else { "s" }),
                ];
                if let Some((_, p)) = client.progress.first_key_value() {
                    let pct = p.percentage.map(|pct| format!(" {pct}%")).unwrap_or_default();
                    parts.push(format!("{}{pct}", p.title));
                }
                parts.join(" · ")
            }
        }
    }

    /// Kills this buffer's instance and lets the next settle spawn a fresh
    /// one — the deliberate act after a crash, a hang, or installing the
    /// binary that was missing.
    fn lsp_restart(&mut self, id: BufferId) {
        // Wiped either way: the point of a manual restart after installing
        // the binary is that the spawn is actually retried.
        self.lsp.clear_failures();
        match &self.entry(id).lsp {
            lsp::Attach::Doc(doc) => {
                let server = doc.server;
                self.lsp.kill_instance(server);
                // Every buffer of that instance re-attaches, which is also
                // what re-opens their documents on the fresh one.
                for entry in &mut self.buffers {
                    if matches!(&entry.lsp, lsp::Attach::Doc(d) if d.server == server) {
                        entry.lsp = lsp::Attach::Unresolved;
                    }
                }
            }
            // Not attached: a fresh verdict, not a shrug — the config or the
            // filesystem may have changed since the last look.
            _ => self.entry_mut(id).lsp = lsp::Attach::Unresolved,
        }
        self.session.status = "lsp: restarting".into();
    }

    /// Shuts this buffer's instance down and keeps it down — until `:lsp
    /// restart`, or a config reload, asks again.
    fn lsp_stop(&mut self, id: BufferId) {
        let lsp::Attach::Doc(doc) = &self.entry(id).lsp else {
            self.session.status = "lsp: nothing to stop".into();
            return;
        };
        let server = doc.server;
        let epoch = self.config_epoch;
        self.lsp.kill_instance(server);
        for entry in &mut self.buffers {
            if matches!(&entry.lsp, lsp::Attach::Doc(d) if d.server == server) {
                entry.lsp =
                    lsp::Attach::No { epoch, reason: "stopped (`:lsp restart` starts it)".into() };
            }
        }
        self.session.status = "lsp: stopped".into();
    }

    /// Registers how LSP reader threads wake the frontend's event loop — the
    /// same handshake as `set_clipboard`: the library does not learn what an
    /// event loop is. Without one, messages wait for the next natural settle,
    /// which is what a headless embedder that pumps on its own schedule wants.
    pub fn set_lsp_waker(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        self.lsp.inbox().set_waker(wake);
    }

    /// Replaces how language servers come to exist — a test's fake, or an
    /// embedding host that has no processes to spawn.
    pub fn set_lsp_spawner(&mut self, spawner: impl lsp::transport::Spawn + 'static) {
        self.lsp.set_spawner(spawner);
    }

    /// Session end: every server asked to leave, briefly waited for, then
    /// made to. The frontend calls this once, after its loop and before the
    /// terminal is handed back.
    pub fn shutdown_lsp(&mut self) {
        self.lsp.shutdown_all();
    }

    /// Installs how git baselines are fetched — [`crate::git::baseline`] from
    /// the TUI, a closure returning fixed text from a test, never anything
    /// from an embedder that does not call this. Fetches for every buffer
    /// already open, so it can be installed after the files are.
    pub fn set_git_baseline(&mut self, loader: impl Fn(&Path) -> Option<String> + 'static) {
        self.git_baseline = Some(Box::new(loader));
        let ids: Vec<BufferId> = self.buffers.iter().map(|b| b.id).collect();
        for id in ids {
            self.refresh_git(id);
        }
    }

    /// Re-reads the baseline and rediffs — the open/revert/save moments, when
    /// the file's relationship to the repository can have moved. Never per
    /// keystroke: edits rediff against the baseline already held, at the
    /// drain.
    fn refresh_git(&mut self, id: BufferId) {
        let Some(loader) = &self.git_baseline else { return };
        let Some(entry) = self.buffers.iter_mut().find(|b| b.id == id) else { return };
        let Some(path) = &entry.buffer.path else {
            entry.git = None;
            return;
        };
        entry.git = loader(path).map(|baseline| {
            let diff = crate::git::diff(&baseline, &entry.buffer.rope().to_string());
            GitState { baseline, seen: entry.buffer.edits(), diff }
        });
    }

    /// The theme's style for a severity.
    fn diag_style(&self, severity: lsp::Severity) -> crate::theme::Style {
        match severity {
            lsp::Severity::Error => self.theme.ui.diag_error,
            lsp::Severity::Warning => self.theme.ui.diag_warning,
            lsp::Severity::Info => self.theme.ui.diag_info,
            lsp::Severity::Hint => self.theme.ui.diag_hint,
        }
    }

    /// The theme's style for a git sign.
    fn git_style(&self, sign: crate::git::Sign) -> crate::theme::Style {
        match sign {
            crate::git::Sign::Add => self.theme.ui.git_add,
            crate::git::Sign::Change => self.theme.ui.git_change,
            crate::git::Sign::Delete | crate::git::Sign::DeleteTop => self.theme.ui.git_delete,
        }
    }

    /// The gutter cell's marks for the rows on screen: `(row, sign, style)`.
    ///
    /// Its own call rather than a decoration, because the gutter has always
    /// been the frontend's to draw — a decoration names columns of the text
    /// area. Two tenants, one cell: the git sign underneath, the diagnostic
    /// over it — the mark that says *wrong* beats the mark that says
    /// *different* — and the worst severity on a row wins its one cell. See
    /// `docs/specs/git-signs.md`.
    pub fn gutter_signs(
        &self,
        window: WindowId,
        rows: std::ops::Range<usize>,
    ) -> Vec<(usize, char, crate::theme::Style)> {
        let Some(Pane::Text { buffer, options, .. }) = self.pane(window) else {
            return Vec::new();
        };
        if options.gutter == 0 {
            return Vec::new();
        }
        let Some(id) = self.window_of(window).and_then(Window::buffer) else { return Vec::new() };
        let mut cells: std::collections::BTreeMap<usize, (char, crate::theme::Style)> =
            Default::default();
        if options.git_signs
            && let Some(git) = self.buffers.iter().find(|b| b.id == id).and_then(|b| b.git.as_ref())
        {
            for &(row, sign) in &git.diff.signs {
                if rows.contains(&row) {
                    cells.insert(row, (sign.glyph(), self.git_style(sign)));
                }
            }
        }
        if options.diagnostics {
            let mut worst: std::collections::BTreeMap<usize, lsp::Severity> = Default::default();
            for d in self.diagnostics(id) {
                let row = buffer.row_at(Cursor::at(d.start));
                if rows.contains(&row) {
                    let sev = worst.entry(row).or_insert(d.severity);
                    *sev = (*sev).min(d.severity);
                }
            }
            for (row, sev) in worst {
                // The underline comes off, as it does on the EOL message: it
                // marks a range of text, and this is a mark about the line.
                let style = crate::theme::Style { underline: false, ..self.diag_style(sev) };
                cells.insert(row, ('•', style));
            }
        }
        cells.into_iter().map(|(row, (sign, style))| (row, sign, style)).collect()
    }

    /// The numstat for the buffer a window shows — `None` where git has no
    /// baseline (or `git_signs` is off), `Some` and clean where it does and
    /// nothing differs. The frontend puts it in the status row; see
    /// `docs/specs/git-signs.md`.
    pub fn git_stats(&self, window: WindowId) -> Option<crate::git::Stats> {
        let Some(Pane::Text { options, .. }) = self.pane(window) else { return None };
        if !options.git_signs {
            return None;
        }
        let id = self.window_of(window).and_then(Window::buffer)?;
        Some(self.buffers.iter().find(|b| b.id == id)?.git.as_ref()?.diff.stats)
    }

    /// The stored diagnostics for a buffer, in buffer order — empty until a
    /// server has spoken, and empty again when it publishes so. The
    /// decorations pass draws them; `]d` / `[d` walk them. See
    /// `docs/specs/diagnostics.md`.
    pub fn diagnostics(&self, id: BufferId) -> &[lsp::Diag] {
        match self.buffers.iter().find(|b| b.id == id).map(|b| &b.lsp) {
            Some(lsp::Attach::Doc(doc)) => &doc.diagnostics,
            _ => &[],
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
        Region::of(buffer, selections, Shape::Block, self.session.block_to_eol)
            .parts()
            .iter()
            .map(|part| (part.start, part.end))
            .collect()
    }

    pub fn block_span_at(&self, row: usize) -> (usize, usize) {
        let (Some(buffer), Some(selections)) = (self.buffer(), self.selections()) else {
            return (0, 0);
        };
        let span = crate::region::block_span_at(buffer, selections, self.session.block_to_eol, row);
        (span.start, span.end)
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
        *self.syntax = syntax_for(self.buffer, self.options);
        // A new path is a new file type, and a new file type is new options:
        // `:w Makefile` has to bring a Makefile's tabs with it.
        *self.filetype = filetype_of(self.buffer);
        *self.options = resolve_options(self.session, *self.filetype, self.buffer.path.as_deref());
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
        // Whatever was lit by the last yank goes out when you do the next
        // thing; the deadline is only for when you do nothing at all.
        self.session.flash = None;
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
                Action::OperateSelection { .. } | Action::PasteSelection { .. } => {
                    Some(self.selection_extent())
                }
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
            Some(Shape::Lines) => {
                let rows = self.buffer.row_at(Cursor::at(hi)) - self.buffer.row_at(Cursor::at(lo));
                Extent::Lines(rows + 1)
            }
            Some(Shape::Block) => {
                let rows = self.buffer.row_at(Cursor::at(hi)) - self.buffer.row_at(Cursor::at(lo));
                let (left, right) = self.block_columns();
                Extent::Block { rows: rows + 1, cols: right + 1 - left }
            }
            _ => Extent::Chars(hi - lo + 1),
        }
    }

    fn block_columns(&self) -> (usize, usize) {
        crate::region::block_columns(self.buffer, self.selections)
    }

    /// What is selected, as a region.
    ///
    /// The one place the mode's shape becomes characters. Every operator over
    /// a selection asks this and then works on what it gets, rather than
    /// re-deriving "what does blockwise mean here" for itself — which is what
    /// left `S(` over a rectangle wrapping everything between the corners.
    fn selected(&self) -> Region {
        Region::of(
            self.buffer,
            self.selections,
            self.session.mode.visual().unwrap_or(Shape::Chars),
            self.session.block_to_eol,
        )
    }

    pub fn block_spans(&self) -> Vec<(usize, usize)> {
        self.selected().parts().iter().map(|part| (part.start, part.end)).collect()
    }

    pub fn block_span_at(&self, row: usize) -> (usize, usize) {
        let span = crate::region::block_span_at(
            self.buffer,
            self.selections,
            self.session.block_to_eol,
            row,
        );
        (span.start, span.end)
    }

    /// The char at `(row, col)`, clamped to the row.
    fn at_row_col(&self, row: usize, col: usize) -> Cursor {
        let row = row.min(self.buffer.line_count() - 1);
        let start = self.buffer.rope().line_to_char(row);
        Cursor::at(start + col.min(self.buffer.line_len(row).saturating_sub(1)))
    }

    /// Cuts or copies the rectangle.
    ///
    /// Not routed through `for_each_selection`, because the selections it would
    /// iterate do not exist yet — the block is derived, and this is the moment
    /// it becomes real.
    fn operate_block(&mut self, op: Operator, sink: Sink) {
        let region = self.selected();
        let spans: Vec<(usize, usize)> =
            region.parts().iter().map(|part| (part.start, part.end)).collect();
        if sink != Sink::BlackHole {
            // One entry, not one per row: what was taken is a rectangle, and
            // pasting it back has to know that. The shape spells the text —
            // see [`Region::text`].
            let entry = Entry { text: region.text(self.buffer), kind: region.shape() };
            self.session.capture(entry, sink);
        }

        if op == Operator::Yank {
            for &(start, end) in &spans {
                if end > start {
                    self.flash(start..end);
                }
            }
        }

        let top_left = region.start();
        if op != Operator::Yank {
            region.cut(self.buffer, op);
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

    /// The entry a paste puts, or `None` with the reason already reported.
    ///
    /// Cloned because pasting borrows the buffer mutably while the entry is
    /// still owned by the ring.
    fn paste_source(&mut self, sink: Sink) -> Option<Entry> {
        match sink {
            // Nothing ever reaches the black hole, so nothing comes out of it.
            // The parser refuses `"_p` before it arrives here; this is the same
            // rule said where the paste happens.
            Sink::BlackHole => None,
            // `clipboard_entry` has already said which of its several ways it
            // came back empty; the ring has only one.
            Sink::System => self.session.clipboard_entry(),
            Sink::Ring => {
                let entry = self.session.registers.front().cloned();
                if entry.is_none() {
                    self.session.status = "nothing to paste".into();
                }
                entry
            }
            // `"np` opens the picker over the names instead of pasting — the
            // paste actions turn into `open_picker` before they get here.
            Sink::Named => None,
        }
    }

    /// Visual mode's `p` — the selection is replaced by `entry`.
    ///
    /// `capture` puts what it displaced on the ring, which is what makes `p` a
    /// swap and `P` a plain overwrite. The ring, never the sink: `"+p` reads
    /// the clipboard, but what it pushed out is ordinary editing history.
    ///
    /// See `docs/specs/registers.md`.
    fn paste_over_selection(&mut self, entry: &Entry, capture: bool, count: usize) {
        if self.session.mode.visual() == Some(Shape::Block) {
            self.paste_over_block(entry, capture, count);
            return;
        }
        let shape = self.session.mode.visual().unwrap_or(Shape::Chars);
        let linewise = shape == Shape::Lines;
        self.for_each_selection(|ed, sel| {
            let part = Region::part_of(ed.buffer, sel, shape);
            // What is displaced goes with its terminator, so the entry can put
            // whole lines back in its place.
            let part = match linewise {
                true => part.terminated(ed.buffer),
                false => part,
            };
            let (start, end) = (part.start, part.end);
            let (removed, landed) = ed.buffer.paste_over(start, end, linewise, entry, count);
            if capture {
                ed.session.registers.push(removed);
            }
            Selection::collapsed(landed)
        });
        self.session.mode = Mode::Normal;
    }

    /// The blockwise case, which is spans rather than a range.
    ///
    /// A charwise entry goes into every row — each span is a range in its own
    /// right, and replacing them one by one is what "paste over this
    /// rectangle" means when the thing being pasted is not one. The other two
    /// kinds cut the rectangle out first and then paste as they always do:
    /// lines below the block, a rectangle at its corner.
    fn paste_over_block(&mut self, entry: &Entry, capture: bool, count: usize) {
        let region = self.selected();
        if capture {
            // One entry, not one per row: what was taken is a rectangle, and
            // pasting it back has to know that.
            let taken = Entry { text: region.text(self.buffer), kind: region.shape() };
            self.session.registers.push(taken);
        }

        let top_left = region.start();
        let bottom = region.parts().last().map_or(0, |part| part.start);
        let last_row = self.buffer.row_at(Cursor::at(bottom));

        let landed = match entry.kind {
            Shape::Chars => {
                // Each part is a range in its own right, and replacing them one
                // by one is what "paste over this rectangle" means when the
                // thing being pasted is not one.
                for part in region.parts().iter().rev() {
                    self.buffer.paste_over(part.start, part.end, false, entry, count);
                }
                self.buffer.clamped(Cursor::at(top_left), false)
            }
            Shape::Lines => {
                region.cut(self.buffer, Operator::Delete);
                let at = self.buffer.at_row(last_row, false);
                self.buffer.paste(at, entry, false, count)
            }
            Shape::Block => {
                region.cut(self.buffer, Operator::Delete);
                let at = self.buffer.clamped(Cursor::at(top_left), true);
                self.buffer.paste(at, entry, true, count)
            }
        };

        self.session.mode = Mode::Normal;
        *self.selections = Selections::single(landed);
    }

    /// Where the block's cursors go for `I` and `A`.
    ///
    /// `I` skips a row that does not reach the left edge; `A` pads one out to
    /// the column so what is appended lines up. Vim pads on `Esc`, bi pads on
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

    /// Re-selects `extent` from the cursor and applies the visual command to
    /// it — the operators, and the paste that replaces what it covers.
    fn repeat_over(&mut self, change: &Change, extent: Extent) {
        if !matches!(
            change.command.action,
            Action::OperateSelection { .. } | Action::PasteSelection { .. }
        ) {
            return;
        }
        let kind = match extent {
            Extent::Chars(_) => Shape::Chars,
            Extent::Lines(_) => Shape::Lines,
            Extent::Block { .. } => Shape::Block,
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
        self.apply(cmd_of(change.command.action.clone()));
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
            // Always intercepted by `Editor::apply` before a view exists —
            // the menu is session state, and a view holds none.
            Action::CompleteNext | Action::CompletePrev => {}
            Action::Move(m) => {
                let Some(m) = self.resolve_find(*m) else { return };
                // `$` in a block is a ragged right edge rather than a column,
                // and any other motion gives the edge back to the head.
                if self.session.mode.visual() == Some(Shape::Block) {
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
            // `>` before the general form: it captures nothing, so none of the
            // register machinery below applies to it, and it is always
            // linewise however its motion is classified.
            Action::Operate { op: Operator::Indent { right }, target, count, .. } => {
                let Some(target) = self.resolve_find_target(*target) else { return };
                let (right, count) = (*right, *count);
                let indent = self.options.indent();
                self.for_each_selection(|ed, sel| {
                    let Some((first, last)) = ed.buffer.target_rows(sel.head, target, count) else {
                        return sel;
                    };
                    match ed.buffer.indent_rows(first, last, right, 1, &indent) {
                        Some(landed) => Selection::collapsed(landed),
                        None => sel,
                    }
                });
            }
            // `=` beside `gq` and `>`: captures nothing, always linewise.
            // See `docs/specs/indent.md`.
            Action::Operate { op: Operator::Reindent, target, count, .. } => {
                let Some(target) = self.resolve_find_target(*target) else { return };
                let count = *count;
                let indent = self.options.indent();
                self.for_each_selection(|ed, sel| {
                    let Some((first, last)) = ed.buffer.target_rows(sel.head, target, count) else {
                        return sel;
                    };
                    match ed.buffer.reindent_rows(first, last, &indent) {
                        Some(landed) => Selection::collapsed(landed),
                        None => sel,
                    }
                });
            }
            // Like `>`: captures nothing, always linewise. See
            // `docs/specs/reflow.md`.
            Action::Operate { op: Operator::Reflow, target, count, .. } => {
                let Some(target) = self.resolve_find_target(*target) else { return };
                let count = *count;
                let (width, tab) = (self.options.textwidth, self.options.tab_width);
                self.for_each_selection(|ed, sel| {
                    let Some((first, last)) = ed.buffer.target_rows(sel.head, target, count) else {
                        return sel;
                    };
                    match ed.buffer.reflow_rows(first, last, width, tab) {
                        Some(landed) => Selection::collapsed(landed),
                        None => sel,
                    }
                });
            }
            Action::Operate { op, target, count, sink } => {
                let Some(target) = self.resolve_find_target(*target) else { return };
                let (op, count, sink) = (*op, *count, *sink);
                self.for_each_selection(|ed, sel| {
                    match ed.buffer.operate(sel.head, op, target, count) {
                        Some((entry, landed, range)) => {
                            ed.session.capture(entry, sink);
                            if op == Operator::Yank {
                                ed.flash(range);
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
            Action::Paste { before, count, sink } => {
                // `"np` is a choice, not a slot: the picker over the names is
                // what pasting from the named space means.
                if *sink == Sink::Named {
                    self.open_picker(PickerKind::Named { before: *before });
                    return;
                }
                let Some(entry) = self.paste_source(*sink) else { return };
                let (before, count) = (*before, *count);
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.paste(sel.head, &entry, before, count))
                });
            }
            Action::PasteSelection { capture, count, sink } => {
                if *sink == Sink::Named {
                    // `capture` is the `p`/`P` distinction here, exactly as
                    // `paste_pick` will read it back out of `before`.
                    self.open_picker(PickerKind::Named { before: !*capture });
                    return;
                }
                let Some(entry) = self.paste_source(*sink) else { return };
                self.paste_over_selection(&entry, *capture, *count);
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
                // A line that ended up as nothing but whitespace was almost
                // certainly opened and thought better of, and the indent
                // autoindent put there is invisible on screen and perfectly
                // visible in the diff. Only when autoindent is on: with it off,
                // nothing but the user put that whitespace there.
                let clearing = stepping_back && self.options.autoindent;
                self.session.mode = Mode::Normal;
                self.session.replaced.clear();
                self.selections.collapse_each();
                if clearing {
                    self.for_each_selection(|ed, sel| {
                        Selection::collapsed(ed.buffer.clear_blank_line(sel.head))
                    });
                }
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
                let indent = self.options.indent();
                self.session.mode = Mode::Insert;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.open_line(sel.head, below, &indent))
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
            Action::InsertText(text) => {
                let text = text.clone();
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.insert_str(sel.head, &text))
                });
            }
            Action::InsertNewline => {
                let indent = self.options.indent();
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.insert_newline(sel.head, &indent))
                });
            }
            Action::Backspace => {
                let indent = self.options.indent();
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.backspace_indent(sel.head, &indent))
                });
            }
            Action::InsertIndent { right } => {
                let (right, indent) = (*right, self.options.indent());
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(match right {
                        true => ed.buffer.insert_indent(sel.head, &indent),
                        false => ed.buffer.remove_indent(sel.head, &indent),
                    })
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
                if *kind == Shape::Block {
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
                // A row at a time: `r` overwrites characters, and a newline is
                // not one it may overwrite.
                let spans = self.selected().spans(self.buffer);
                // Length-preserving, so the order does not matter and no shift
                // correction is needed — unlike every other edit here.
                for span in spans {
                    self.buffer.replace_chars(Cursor::at(span.start), ch, span.end - span.start);
                }
                // Every selection collapses onto its own start, which keeps a
                // multi-cursor visual `r` multi-cursor.
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.clamped(Cursor::at(sel.range().0), false))
                });
                self.session.mode = Mode::Normal;
            }
            // Whole rows whatever the shape of the selection — a block `>`
            // shifts the lines it touches, as vim's does.
            //
            // The selection survives, which vim drops and every vimrc puts
            // back with `vnoremap > >gv`. It is also what makes `3>` fall out
            // for free: three steps is the command run three times, and it can
            // only run three times if there is still a selection.
            Action::OperateSelection { op: Operator::Indent { right }, .. } => {
                let (right, indent) = (*right, self.options.indent());
                self.for_each_selection(|ed, sel| {
                    let (lo, hi) = sel.range();
                    let first = ed.buffer.row_at(Cursor::at(lo));
                    let last = ed.buffer.row_at(Cursor::at(hi));
                    if ed.buffer.indent_rows(first, last, right, 1, &indent).is_none() {
                        return sel;
                    }
                    // The rows it touched, whole: the text moved out from under
                    // the old columns, so keeping them would slide the
                    // selection sideways under a repeated `>`.
                    let backwards = sel.head.at < sel.anchor.at;
                    let start = ed.buffer.at_row(first, false);
                    let end = ed.buffer.line_end(ed.buffer.at_row(last, false), false);
                    match backwards {
                        true => Selection { anchor: end, head: start },
                        false => Selection { anchor: start, head: end },
                    }
                });
            }
            Action::OperateSelection { op: Operator::Reindent, .. } => {
                let indent = self.options.indent();
                self.for_each_selection(|ed, sel| {
                    let (lo, hi) = sel.range();
                    let first = ed.buffer.row_at(Cursor::at(lo));
                    let last = ed.buffer.row_at(Cursor::at(hi));
                    match ed.buffer.reindent_rows(first, last, &indent) {
                        Some(landed) => Selection::collapsed(landed),
                        None => Selection::collapsed(sel.head),
                    }
                });
                self.session.mode = Mode::Normal;
            }
            // The rows the selection touches, whatever its shape — there is
            // no reflowing half a line. Unlike `>` it consumes the selection:
            // the text under it has been rewrapped out of recognition.
            Action::OperateSelection { op: Operator::Reflow, .. } => {
                let (width, tab) = (self.options.textwidth, self.options.tab_width);
                self.for_each_selection(|ed, sel| {
                    let (lo, hi) = sel.range();
                    let first = ed.buffer.row_at(Cursor::at(lo));
                    let last = ed.buffer.row_at(Cursor::at(hi));
                    match ed.buffer.reflow_rows(first, last, width, tab) {
                        Some(landed) => Selection::collapsed(landed),
                        None => Selection::collapsed(sel.head),
                    }
                });
                self.session.mode = Mode::Normal;
            }
            Action::OperateSelection { op, sink }
                if self.session.mode.visual() == Some(Shape::Block) =>
            {
                self.operate_block(*op, *sink);
            }
            Action::OperateSelection { op, sink } => {
                let (op, sink) = (*op, *sink);
                let shape = self.session.mode.visual().unwrap_or(Shape::Chars);
                let linewise = shape == Shape::Lines;
                self.for_each_selection(|ed, sel| {
                    let part = Region::part_of(ed.buffer, sel, shape);
                    // Change keeps the line for insert mode to sit on, the same
                    // rule `cc` follows; delete and yank take the terminator.
                    let part = match linewise && op != Operator::Change {
                        true => part.terminated(ed.buffer),
                        false => part,
                    };
                    let (start, end) = (part.start, part.end);
                    match ed.buffer.operate_range(sel.head, op, start, end, linewise) {
                        Some((entry, landed, range)) => {
                            ed.session.capture(entry, sink);
                            if op == Operator::Yank {
                                ed.flash(range);
                            }
                            Selection::collapsed(landed)
                        }
                        None => Selection::collapsed(sel.head),
                    }
                });
                self.session.mode =
                    if op == Operator::Change { Mode::Insert } else { Mode::Normal };
            }

            Action::Surround { target, count, with } => {
                let Some(target) = self.resolve_find_target(*target) else { return };
                let (count, with) = (*count, *with);
                let Some(pair) = crate::surround::pair_for(with) else {
                    self.session.status = format!("nothing surrounds with {with}");
                    return;
                };
                self.for_each_selection(|ed, sel| {
                    match ed.buffer.range_of(sel.head, target, count) {
                        Some((start, end)) => {
                            Selection::collapsed(ed.buffer.surround(sel.head, start, end, &pair))
                        }
                        None => sel,
                    }
                });
            }
            // Every part of what is selected, whatever shape it has. A
            // rectangle wraps each row's columns — it used to wrap everything
            // between the two corners, brackets and line ends included, which
            // is what a charwise range means and a rectangle never did.
            Action::SurroundSelection { with } => {
                let with = *with;
                let Some(pair) = crate::surround::pair_for(with) else {
                    self.session.status = format!("nothing surrounds with {with}");
                    return;
                };
                let region = self.selected();
                let before = self.selections.as_pairs();
                // Back to front: an insertion shifts everything below it.
                let mut heads = Vec::new();
                for part in region.parts().iter().rev() {
                    if part.is_empty() {
                        continue;
                    }
                    let at = Cursor::at(part.start);
                    heads.push(self.buffer.surround(at, part.start, part.end, &pair));
                }
                if heads.is_empty() {
                    self.session.status = "nothing selected".into();
                    return;
                }
                self.selections.set(heads.into_iter().map(Selection::collapsed).collect());
                self.buffer.commit_undo(before, self.selections.as_pairs());
                self.session.mode = Mode::Normal;
            }
            Action::Unsurround { of } => {
                let of = *of;
                let mut missing = false;
                self.for_each_selection(|ed, sel| match ed.buffer.unsurround(sel.head, of) {
                    Some(landed) => Selection::collapsed(landed),
                    None => {
                        missing = true;
                        sel
                    }
                });
                if missing {
                    self.session.status = format!("no {of} around the cursor");
                }
            }
            Action::Resurround { of, with } => {
                let (of, with) = (*of, *with);
                let Some(pair) = crate::surround::pair_for(with) else {
                    self.session.status = format!("nothing surrounds with {with}");
                    return;
                };
                let mut missing = false;
                self.for_each_selection(|ed, sel| {
                    match ed.buffer.resurround(sel.head, of, &pair) {
                        Some(landed) => Selection::collapsed(landed),
                        None => {
                            missing = true;
                            sel
                        }
                    }
                });
                if missing {
                    self.session.status = format!("no {of} around the cursor");
                }
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
                if let Some(selection) = self.next_match_selection() {
                    self.selections.push(selection);
                }
            }
            // The same search, over the primary rather than beside it. `push`
            // makes what it adds primary, so the one this replaces is always
            // the most recently placed cursor — which is the one the user is
            // looking at and the only one they can mean by "not this".
            Action::SkipCursorToNextMatch => {
                if let Some(selection) = self.next_match_selection() {
                    *self.selections.primary_mut() = selection;
                }
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

            Action::MoveLines { down } => {
                let (first, last) = self.selected_rows();
                let to = match down {
                    true => first + 1,
                    false => first.saturating_sub(1),
                };
                self.move_lines(first, last, to);
            }
            Action::MoveLinesTo { row } => {
                let (first, last) = self.selected_rows();
                self.move_lines(first, last, *row);
            }

            Action::ScrollLine { down } => self.scroll_by(if *down { 1 } else { -1 }, false),
            Action::ScrollHalfPage { down } => {
                let half = (*self.height / 2).max(1) as isize;
                self.scroll_by(if *down { half } else { -half }, true);
            }

            // Handled by `Editor` before this view was built: the window
            // tree, the buffer list, the command line and the picker.
            Action::EnterCommandMode
            | Action::Ex { .. }
            | Action::BoundaryJump { .. }
            | Action::CommandChar(_)
            | Action::CommandBackspace
            | Action::CommandMove(_)
            | Action::CommandRecall { .. }
            | Action::CommandCancel
            | Action::CommandExecute
            | Action::PickChar(_)
            | Action::PickBackspace
            | Action::PickNext
            | Action::PickPrev
            | Action::PickToggleShort
            | Action::PickCancel
            | Action::PickAccept
            | Action::LabelChar(_)
            | Action::LabelCancel
            | Action::ShowScopes
            | Action::EnterFind
            | Action::FindChar(_)
            | Action::FindBackspace
            | Action::FindCancel
            | Action::Buffer(_)
            | Action::Window(_)
            | Action::Tree(_)
            | Action::Results(_) => {}
        }
    }

    /// Where a match-following cursor goes next: the primary selection, or
    /// the word under it, found again after where it sits.
    ///
    /// `None` means it did not move and the status line already says why. The
    /// two callers differ only in what they do with the answer — `<C-n>` adds
    /// a cursor there, `<C-x>` moves the one it has — and that difference is
    /// the whole of "skip": the match is passed over rather than taken.
    fn next_match_selection(&mut self) -> Option<Selection> {
        let primary = self.selections.primary();
        // The selection itself when there is one, otherwise the word under
        // the cursor — so it works in both normal and visual.
        let (start, end) = if primary.is_collapsed() {
            match self.buffer.word_at(primary.head) {
                Some(range) => range,
                None => {
                    self.session.status = "no word under the cursor".into();
                    return None;
                }
            }
        } else {
            primary.inclusive_range(self.buffer.rope().len_chars())
        };

        let needle = self.buffer.slice(start, end);
        let found = match self.buffer.find_next(primary.head.at, &needle) {
            Some(found) => found,
            None => {
                self.session.status = format!("no more matches for \"{needle}\"");
                return None;
            }
        };
        if found == start {
            self.session.status = "only one match".into();
            return None;
        }

        let width = needle.chars().count();
        // A selection with room in it is only meaningful in visual mode. In
        // normal mode the cursor goes to the *start* of the match: collapsing
        // the range onto its head would leave it on the last character, so
        // typing would land inside the word rather than in front of it.
        Some(match self.session.mode.visual() {
            Some(_) => Selection { anchor: Cursor::at(found), head: Cursor::at(found + width - 1) },
            None => Selection::at(found),
        })
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
        let badge = |kind: Shape| match kind {
            Shape::Lines => Some('¶'),
            Shape::Block => Some('▚'),
            Shape::Chars => None,
        };
        // Short entries hide behind `Ctrl-A` on the ring, where `x` deletes
        // bury the list; a name is short *because* someone chose it.
        let (items, min_len): (Vec<Item>, usize) = match kind {
            PickerKind::Named { .. } => {
                if self.session.registers.named().is_empty() {
                    // An empty overlay is a worse answer than saying so.
                    self.session.status = "no named registers".into();
                    return;
                }
                let items = self
                    .session
                    .registers
                    .named()
                    .iter()
                    // The name first, so it is the row; the entry follows and
                    // rides in the preview under it.
                    .map(|(name, e)| Item {
                        text: format!("{name}\n{}", e.text),
                        badge: badge(e.kind),
                    })
                    .collect();
                (items, 0)
            }
            _ => {
                if self.session.registers.is_empty() {
                    self.session.status = "nothing to paste".into();
                    return;
                }
                let items = self
                    .session
                    .registers
                    .iter()
                    .map(|e| Item { text: e.text.clone(), badge: badge(e.kind) })
                    .collect();
                (items, REGISTER_MIN_LEN)
            }
        };
        self.session.picker = Some(Picker::new(kind, items, min_len));
        // Remembered rather than dropped: the selection is what the chosen
        // entry will replace, and `Mode::Pick` is about to hide that it exists.
        self.session.pick_from = Some(std::mem::replace(&mut self.session.mode, Mode::Pick));
    }

    /// Pastes the register the picker landed on.
    ///
    /// Over a selection it replaces it, exactly as `p` does — the picker only
    /// chooses which entry, and `before` is the `p`/`P` distinction whichever
    /// of the two pastes it turns into.
    fn paste_pick(&mut self, chosen: usize, before: bool) {
        let Some(entry) = self.session.registers.get(chosen).cloned() else { return };
        self.paste_chosen(entry, before);
    }

    /// The same, out of the named space — the entry the picker's row stands
    /// for, by position in the same most-recent-first order.
    fn paste_named(&mut self, chosen: usize, before: bool) {
        let Some(entry) = self.session.registers.named_at(chosen).cloned() else { return };
        self.paste_chosen(entry, before);
    }

    fn paste_chosen(&mut self, entry: Entry, before: bool) {
        // Push before pasting: move-to-front makes this the ring's head, so
        // `.` and a later bare `p` repeat the entry you chose rather than
        // whatever happened to be most recent.
        self.session.registers.push(entry.clone());
        if self.session.mode.visual().is_some() {
            self.paste_over_selection(&entry, !before, 1);
            return;
        }
        let landed = self.buffer.paste(self.selections.cursor(), &entry, before, 1);
        *self.selections = Selections::single(landed);
    }

    /// The `:` commands. Deliberately tiny — this is not where the editor gets
    /// interesting, and a real command table wants the config layer first.
    /// The rows the move acts on: the selected block, or the cursor's line.
    /// What a `:` command with no scope of its own acts on.
    ///
    /// Beside the commands rather than inside each one: "no range means the
    /// cursor's line" is a decision, and four commands making it privately is
    /// four places to look when they disagree.
    fn fallback_region(&self, fallback: Fallback) -> Region {
        match fallback {
            // Renaming a name is what `:case` is for, and selecting it first
            // is a keystroke that says nothing new. Every cursor gets one, so
            // a column of cursors respells a column of names.
            Fallback::Words => Region::spanning(
                Shape::Chars,
                self.selections.all().iter().filter_map(|selection| {
                    self.buffer
                        .word_at(selection.head)
                        // `iw` on whitespace is the run of whitespace, which is
                        // right for `diw` and is not a word to respell.
                        .filter(|&(a, b)| {
                            self.buffer.slice(a, b).chars().any(char::is_alphanumeric)
                        })
                }),
            ),
            Fallback::CursorRow => {
                let row = self.buffer.row_at(self.selections.cursor());
                Region::of_rows(self.buffer, row, row)
            }
            Fallback::File => {
                Region::of_rows(self.buffer, 0, self.buffer.line_count().saturating_sub(1))
            }
            Fallback::SelectionRows => {
                let (first, last) = self.selected_rows();
                Region::of_rows(self.buffer, first, last)
            }
        }
    }

    /// The region a `:` command acts on — the one place a scope becomes spans.
    ///
    /// `shape` is the selection's shape when the `:` line was opened, passed
    /// in rather than read off the mode. The mode is gone by the time a
    /// command runs — `CommandExecute` takes it out of the session before
    /// dispatching — and reading it here is exactly how a rectangle used to
    /// arrive at `:case` as a set of whole lines.
    fn region(
        &self,
        scope: Option<Scope>,
        shape: Option<Shape>,
        fallback: Fallback,
    ) -> Result<Region, String> {
        match scope {
            Some(Scope::Selection) => Ok(Region::of(
                self.buffer,
                self.selections,
                shape.unwrap_or(Shape::Chars),
                self.session.block_to_eol,
            )),
            Some(Scope::Lines(range)) => {
                let (first, last) = range.rows(self.line_numbers())?;
                Ok(Region::of_rows(self.buffer, first, last))
            }
            None => Ok(self.fallback_region(fallback)),
        }
    }

    /// The rows a region covers, for a command that can only work in whole
    /// lines. Says so when it had to widen, rather than doing it quietly.
    fn whole_rows(&mut self, region: Region) -> Option<(usize, usize)> {
        let (first, last) = region.row_range(self.buffer)?;
        // Said out loud rather than done quietly: a command that can only work
        // in whole lines, handed a rectangle, has widened what you asked for.
        if region != Region::of_rows(self.buffer, first, last) {
            self.session.status = "whole lines".into();
        }
        Some((first, last))
    }

    fn selected_rows(&self) -> (usize, usize) {
        let (lo, hi) = self.selections.primary().range();
        (self.buffer.row_at(Cursor::at(lo)), self.buffer.row_at(Cursor::at(hi)))
    }

    /// Moves the block and takes the cursor — and the selection — with it.
    ///
    /// Keeping the selection is what makes nudging a block a matter of holding
    /// a key rather than counting rows first.
    fn move_lines(&mut self, first: usize, last: usize, to: usize) {
        let col = self.buffer.col_at(self.selections.cursor());
        let selected = !self.selections.primary().is_collapsed();
        let Some(landed) = self.buffer.move_lines(first, last, to, col) else { return };

        if !selected {
            *self.selections = Selections::single(landed);
            return;
        }
        // The block kept its height, so the tail follows the head.
        let head = self.buffer.row_at(landed);
        let end = self.buffer.at_row(head + (last - first), false);
        let (from, upto) = self.buffer.line_range(landed.at, end.at, false);
        self.selections.set(vec![Selection {
            anchor: Cursor::at(from),
            head: Cursor::at(upto.saturating_sub(1)),
        }]);
    }

    /// `:[scope]m {address}` — vim's move, arithmetic and all.
    ///
    /// The scope says which lines; with none written, the selection does,
    /// which is what makes `Shift-Down` and a bare `:m +1` agree about what
    /// they are moving. A scope that is not whole lines is widened to the rows
    /// it touches and says so: there is no moving half a row.
    fn move_to(&mut self, scope: Option<Scope>, shape: Option<Shape>, to: Address) {
        let at = self.line_numbers();
        let region = match self.region(scope, shape, Fallback::SelectionRows) {
            Ok(region) => region,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };
        let Some((first, last)) = self.whole_rows(region) else { return };
        let lines = self.buffer.line_count() as isize;
        // `.` is still the cursor's line even with a range in front of the
        // command — a range does not move the cursor, which is why `:m +1`
        // over a selection depends on which end the cursor is at. Vim's
        // behaviour, and the reason `Shift-Down` exists for the job.
        let address = to.resolve(at);

        // Off either end is refused rather than clamped, because that is what
        // vim does — and unlike the arrow keys, a typed address is a claim
        // about a line that either exists or does not. Zero is not off the
        // end: it is the line to land after that puts the block at the top.
        if address < 0 || address > lines {
            self.session.status = format!("no line {address}");
            return;
        }
        let row = self.after_line(address as usize, first, last);
        self.move_lines(first, last, row);
    }

    /// `:[scope]case {style}` — respells what the scope names, or the word
    /// under each cursor when nothing does.
    ///
    /// The word is the fallback because renaming one is what this is for, and
    /// selecting it first is a keystroke that says nothing new.
    ///
    /// One walk over one region, whatever shape it has: `'v` over a rectangle
    /// respells its columns, `'v` over a charwise selection respells exactly
    /// what is highlighted, and an address names whole rows. The shape is not
    /// consulted here at all — [`View::region`] already spent it.
    fn recase(&mut self, scope: Option<Scope>, shape: Option<Shape>, style: crate::case::Style) {
        let region = match self.region(scope, shape, Fallback::Words) {
            Ok(region) => region,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };
        if region.is_empty() {
            self.session.status = match scope {
                None => "no word under the cursor".into(),
                Some(_) => "nothing to respell".into(),
            };
            return;
        }

        let before = self.selections.as_pairs();
        let rows = region.filled_rows(self.buffer);
        let edits = region.rewrite_rows(self.buffer, |text| crate::case::convert(text, style));

        // Carried across the edits rather than replaced: the cursors were
        // where you put them, and respelling the text under one is not a
        // reason to move it somewhere else. `Edit::map` is the same carry
        // `retab` and the trimmer use.
        let carried: Vec<Selection> = self
            .selections
            .all()
            .iter()
            .map(|selection| {
                let head = Region::carry(&edits, selection.head.at);
                Selection::collapsed(self.buffer.clamped(Cursor::at(head), false))
            })
            .collect();
        self.selections.set(carried);
        self.buffer.commit_undo(before, self.selections.as_pairs());

        if matches!(scope, Some(Scope::Lines(_))) {
            self.session.status =
                format!("{rows} line{} recased", if rows == 1 { "" } else { "s" });
        }
        // A selection that has been rewritten is not a selection any more.
        self.session.mode = Mode::Normal;
    }

    /// `:[scope]s/old/new/flags`.
    ///
    /// Hands back the pattern that ran, so the caller can make it the last
    /// search — `None` when nothing was replaced and there is nothing to
    /// repeat. See `docs/specs/substitute.md`.
    fn substitute(
        &mut self,
        scope: Option<Scope>,
        shape: Option<Shape>,
        how: &crate::substitute::Substitute,
        last_search: Option<String>,
    ) -> Option<String> {
        // An empty pattern is the last thing you searched for, which is what
        // makes `/foo` then `:%s//bar/g` the pair everyone uses.
        let pattern = match how.pattern.is_empty() {
            false => how.pattern.clone(),
            true => match last_search {
                Some(pattern) if !pattern.is_empty() => pattern,
                _ => {
                    self.session.status = "no previous search".into();
                    return None;
                }
            },
        };

        // No scope is the cursor's line, which is vim and is why `%` is the
        // most typed character in the command.
        let region = match self.region(scope, shape, Fallback::CursorRow) {
            Ok(region) => region,
            Err(message) => {
                self.session.status = message;
                return None;
            }
        };

        // Every match first, then the writes: a replacement must not be
        // searched for the pattern it just produced, so `:%s/a/aa/g` doubles
        // each `a` and stops rather than chasing its own output.
        //
        // Per span rather than per row, which is what makes `:'v s/…` over a
        // rectangle stay inside the columns: a span is a row's worth of the
        // region, and searching it is the same search either way.
        let mut hits: Vec<(usize, usize)> = Vec::new();
        let mut rows = 0usize;
        let spans = region.spans(self.buffer);
        let mut end_row = spans.first().map_or(0, |span| span.row);
        for span in &spans {
            let found =
                self.buffer.matches_in_cased(span.start, span.end, &pattern, false, how.case);
            let found = match how.all {
                true => found,
                false => found.into_iter().take(1).collect(),
            };
            if found.is_empty() {
                continue;
            }
            rows += 1;
            end_row = span.row;
            hits.extend(found);
        }
        hits.sort_unstable();

        if hits.is_empty() {
            self.session.status = format!("pattern not found: {pattern}");
            return None;
        }
        let report = format!(
            "{} substitution{} on {} line{}",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" },
            rows,
            if rows == 1 { "" } else { "s" },
        );

        // `n` answers "how many are there" without running the thing and
        // pressing `u`.
        if how.count_only {
            self.session.status = report;
            return Some(pattern);
        }

        let before = self.selections.as_pairs();
        // Back to front, so an earlier replacement cannot move a later one's
        // offsets.
        for &(start, stop) in hits.iter().rev() {
            self.buffer.replace_range(start, stop, &how.replacement);
        }
        // On the first column of the last line changed, which is vim.
        let landing =
            self.buffer.clamped(Cursor::at(self.buffer.rope().line_to_char(end_row)), false);
        *self.selections = Selections::single(landing);
        self.buffer.commit_undo(before, self.selections.as_pairs());

        self.session.status = report;
        // A selection that has been rewritten under you is not a selection.
        self.session.mode = Mode::Normal;
        Some(pattern)
    }

    /// Where a block starts once it is put *after* one-based line `address` —
    /// vim's `:m {number}`, arithmetic and all.
    ///
    /// Which is why it is direction-dependent, and why that is not a bug: the
    /// address names a line in the buffer as it stands now, so a block coming
    /// from above it leaves a hole that the address falls through. From below,
    /// nothing above the address moved and the block lands right after it.
    fn after_line(&self, address: usize, first: usize, last: usize) -> usize {
        let height = last - first + 1;
        // How many of the block's own rows sit at or above the address, and so
        // are not there to be counted once it has been lifted out.
        let lifted = address.saturating_sub(first).min(height);
        address - lifted
    }

    /// The three line numbers an address resolves against, one-based.
    ///
    /// Gathered here so `crate::range` never learns what a `Buffer` is — the
    /// same division `Buffer` and `Indent` draw. See `docs/specs/ranges.md`.
    fn line_numbers(&self) -> Where {
        let (first, last) = self.selected_rows();
        Where {
            lines: self.buffer.line_count(),
            cursor: self.buffer.row_at(self.selections.cursor()) + 1,
            selection: (first + 1, last + 1),
        }
    }

    /// `:42`, `:$`, `:%` — a range and no command puts the cursor on its last
    /// line.
    ///
    /// Clamped rather than refused, unlike a range's own lines: this is the
    /// oldest spelling of "take me there", `:0` has always meant the top, and
    /// a number past the end means the bottom to everyone who types one.
    fn goto(&mut self, address: Address) {
        let row = address.resolve(self.line_numbers()).max(1) as usize;
        self.goto_row(row);
    }

    /// Puts the cursor on a char offset.
    ///
    /// On the character rather than the row, which is what `:symbols` wants:
    /// column zero of a line you can already see is not where the name is.
    fn goto_char(&mut self, at: usize) {
        *self.selections = Selections::single(self.buffer.clamped(Cursor::at(at), false));
    }

    /// Puts the cursor on a one-based row.
    fn goto_row(&mut self, row: usize) {
        let cursor = self.buffer.at_row(row.saturating_sub(1), false);
        *self.selections = Selections::single(cursor);
    }

    /// Returns whether the write succeeded.
    fn write(&mut self, path: &str) -> bool {
        self.trim_for_write();
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
                // For LSP `didSave` — drained by `Editor::settle`, which also
                // notices when the path moved under the document.
                self.session.pending_saves.push(self.id);
                true
            }
            Err(e) => {
                self.session.status = format!("error: {e:#}");
                false
            }
        }
    }

    /// Lights up what a yank just read, until the moment named by
    /// `yank_flash`.
    ///
    /// Appends rather than replaces: one command can yank at several cursors,
    /// and a rectangle is one span per row. What clears the last flash is the
    /// *next command*, in `apply` — see `docs/specs/flash.md`.
    fn flash(&mut self, range: std::ops::Range<usize>) {
        let ms = self.options.yank_flash;
        if ms == 0 || range.is_empty() {
            return;
        }
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms as u64);
        match &mut self.session.flash {
            Some(flash) if flash.buffer == self.id => {
                flash.ranges.push(range);
                flash.until = until;
            }
            slot => *slot = Some(Flash { buffer: self.id, ranges: vec![range], until }),
        }
    }

    /// `:retab` — rewrites the indentation to whatever the options in force
    /// say it should be.
    ///
    /// **The options in force, not the `.editorconfig`.** They are usually the
    /// same thing, and where they are not it is because you said so: the
    /// project's file is one layer of five, and `:set expandtab false` sits
    /// above it (`docs/specs/options.md`). A command that read the project's
    /// file directly would be the one place in bi where an explicit `:set` did
    /// nothing, and you would have to read the source to find out why.
    ///
    /// **Not on write.** Trimming touches the lines you edited; this touches
    /// every indented line in the file, and turning a one-line fix into a
    /// whole-file diff is not a thing a save should do behind you.
    /// `:[scope]sort [flags]` — orders the rows the scope names, the whole
    /// file when nothing narrows it.
    ///
    /// Whole rows only: there is no sorting half a line, so a scope that is
    /// not whole rows widens to the rows it touches and says so, exactly as
    /// `:m` and `:retab` do. The ordering itself lives in `crate::sort`,
    /// which has never heard of a buffer. See `docs/specs/sort.md`.
    fn sort_rows(&mut self, scope: Option<Scope>, shape: Option<Shape>, how: &crate::sort::Sort) {
        let region = match self.region(scope, shape, Fallback::File) {
            Ok(region) => region,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };
        let Some((first, last)) = self.whole_rows(region) else { return };
        if last - first < 1 {
            self.session.status = "nothing to sort".into();
            return;
        }

        let lines: Vec<String> = (first..=last).map(|row| self.buffer.line(row)).collect();
        let (sorted, dropped) = crate::sort::sort_lines(lines.clone(), how);
        if sorted == lines {
            // No edit and no undo entry: an unchanged buffer with a revision
            // in its history is a `u` that appears to do nothing.
            self.session.status = "already sorted".into();
            return;
        }

        let before = self.selections.as_pairs();
        let start = self.buffer.rope().line_to_char(first);
        let stop = self.buffer.rope().line_to_char(last) + self.buffer.line_len(last);
        self.buffer.replace_range(start, stop, &sorted.join("\n"));

        // The block starts here, and the selection that named it has been
        // consumed.
        *self.selections = Selections::single(self.buffer.clamped(Cursor::at(start), false));
        self.buffer.commit_undo(before, self.selections.as_pairs());

        let rows = last - first + 1;
        let mut report = format!("{rows} line{} sorted", if rows == 1 { "" } else { "s" });
        if dropped > 0 {
            report.push_str(&format!(
                ", {dropped} duplicate{} dropped",
                if dropped == 1 { "" } else { "s" }
            ));
        }
        self.session.status = report;
    }

    fn retab(&mut self, scope: Option<Scope>, shape: Option<Shape>) {
        // No scope is the whole file — see [`ExLine::Retab`]. Indentation is
        // a property of a line, so a scope that is not made of whole lines is
        // widened to the rows it touches and says so.
        let region = match self.region(scope, shape, Fallback::File) {
            Ok(region) => region,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };
        let Some((first, last)) = self.whole_rows(region) else { return };

        let before = self.selections.as_pairs();
        let (rows, edits) = self.buffer.retab(first, last, &self.options.indent());
        if rows == 0 {
            // Which is the answer to "is this file already conformant", and
            // worth saying out loud: an empty status after a command that
            // rewrites files reads as "did that work?".
            self.session.status = "indentation is already what the options ask for".into();
            return;
        }

        // Mapped rather than clamped, for the reason `trim_for_write` maps:
        // the cursor was on a line, and it is still on that line.
        let across = |at: usize| edits.iter().fold(at, |at, edit| edit.map(at));
        let mapped: Vec<Selection> = self
            .selections
            .all()
            .iter()
            .map(|selection| Selection {
                anchor: self.buffer.clamped(Cursor::at(across(selection.anchor.at)), false),
                head: self.buffer.clamped(Cursor::at(across(selection.head.at)), false),
            })
            .collect();
        self.selections.set(mapped);
        self.buffer.commit_undo(before, self.selections.as_pairs());

        let how = match self.options.expandtab {
            true => "spaces",
            false => "tabs",
        };
        self.session.status =
            format!("{rows} line{} retabbed to {how}", if rows == 1 { "" } else { "s" });
    }

    /// `:[range]d` — the rows, gone. The cursor's line by default. See
    /// `docs/specs/global.md`.
    /// `:yname {register}` with a range or a selection — a scoped yank into
    /// the named space. The region's own shape travels with the text, so a
    /// charwise selection pastes back inline and a line range as lines.
    fn yank_named(&mut self, scope: Option<Scope>, shape: Option<Shape>, name: &str) {
        let region = match self.region(scope, shape, Fallback::CursorRow) {
            Ok(region) => region,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };
        let entry = Entry { text: region.text(self.buffer), kind: region.shape() };
        self.session.registers.set_named(name, entry);
        self.session.status = format!("named \"{name}\"");
    }

    fn delete_rows(&mut self, scope: Option<Scope>, shape: Option<Shape>) {
        let region = match self.region(scope, shape, Fallback::CursorRow) {
            Ok(region) => region,
            Err(message) => {
                self.session.status = message;
                return;
            }
        };
        let Some((first, last)) = self.whole_rows(region) else { return };

        let before = self.selections.as_pairs();
        let start = self.buffer.rope().line_to_char(first);
        let stop = self.buffer.rope().line_to_char(last + 1);
        self.buffer.replace_range(start, stop, "");

        // Column 0 of the line that moved up into the gap, clamped to the
        // file that is left.
        *self.selections = Selections::single(self.buffer.clamped(Cursor::at(start), false));
        self.buffer.commit_undo(before, self.selections.as_pairs());

        let rows = last - first + 1;
        self.session.status = format!("{rows} fewer line{}", if rows == 1 { "" } else { "s" });
        // A selection that has been deleted out from under you is not a
        // selection any more.
        self.session.mode = Mode::Normal;
    }

    /// The rows of the range that hold — or, inverted, lack — a match:
    /// `:g`'s scan, finished before its first command runs. See
    /// `docs/specs/global.md`.
    fn global_rows(
        &mut self,
        scope: Option<Scope>,
        shape: Option<Shape>,
        pattern: &str,
        invert: bool,
    ) -> Result<Vec<usize>, String> {
        let region = self.region(scope, shape, Fallback::File)?;
        let Some((first, last)) = self.whole_rows(region) else { return Ok(Vec::new()) };
        let mut rows = Vec::new();
        for row in first..=last {
            let start = self.buffer.rope().line_to_char(row);
            let end = start + self.buffer.line_len(row);
            // Smartcase, the same reading `/` and `:s` give a pattern.
            let hit = !self.buffer.matches_in_cased(start, end, pattern, false, None).is_empty();
            if hit != invert {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    /// The whole rows a scope names — `:normal`'s walk.
    fn scope_rows(
        &mut self,
        scope: Option<Scope>,
        shape: Option<Shape>,
    ) -> Result<(usize, usize), String> {
        let region = self.region(scope, shape, Fallback::CursorRow)?;
        self.whole_rows(region).ok_or_else(|| "nothing selected".into())
    }

    /// Tidies the text before it goes to disk, and carries the cursors across
    /// what it removed.
    ///
    /// Before the bytes go out rather than on the way past them, so that what
    /// was written and what is in the buffer are the same text — which is the
    /// property that keeps "modified" and the undo history honest. Mapped
    /// rather than clamped, so a cursor on line 400 does not move because
    /// three blank lines went from the top of the file; other windows on the
    /// same buffer follow through `settle`, like every other edit.
    ///
    /// See `docs/specs/trim.md`.
    fn trim_for_write(&mut self) {
        if !self.options.trim.does_anything() {
            return;
        }
        let edits = self.buffer.trim(&self.options.trim);
        if edits.is_empty() {
            return;
        }
        let across = |at: usize| edits.iter().fold(at, |at, edit| edit.map(at));
        let mapped: Vec<Selection> = self
            .selections
            .all()
            .iter()
            .map(|selection| Selection {
                anchor: self.buffer.clamped(Cursor::at(across(selection.anchor.at)), false),
                head: self.buffer.clamped(Cursor::at(across(selection.head.at)), false),
            })
            .collect();
        self.selections.set(mapped);
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

        self.scroll_to_cursor_col(row);
    }

    /// The horizontal half: keeps the cursor's display column inside the
    /// width the frontend last reported, minus the gutter it draws first.
    ///
    /// Nothing scrolls by chars here — the offset is display columns, so a
    /// tab or a CJK char scrolls by what it occupies rather than by one.
    fn scroll_to_cursor_col(&mut self, row: usize) {
        let gutter = match self.session.zen {
            true => 0,
            false => self.options.gutter_width(self.buffer.line_count()),
        };
        let width = self.width.saturating_sub(gutter);
        if width == 0 {
            // A width the frontend never reported (a test, a headless
            // embedder) means no viewport to stay inside — and clamping to a
            // zero-wide one would pin `left` to the cursor.
            return;
        }
        let line = self.buffer.line(row);
        let col = crate::indent::display_col(
            &line,
            self.buffer.col_at(self.selections.cursor()),
            self.options.tab_width,
        );
        let margin = Self::margin(width);

        if col < (*self.left) + margin {
            (*self.left) = col.saturating_sub(margin);
        } else if col + margin >= (*self.left) + width {
            (*self.left) = col + margin + 1 - width;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Cursor;
    use crate::motion::TextObject;
    use crate::picker::PickerKind;

    /// A config source that serves a string, so a test needs no filesystem.
    struct ConfigText(Option<&'static str>);

    impl crate::config::ConfigSource for ConfigText {
        fn config(&self) -> anyhow::Result<Option<String>> {
            Ok(self.0.map(str::to_string))
        }
    }

    /// A source with both layers, for the project-config tests.
    struct TwoLayers {
        main: Option<&'static str>,
        local: Option<&'static str>,
    }

    impl crate::config::ConfigSource for TwoLayers {
        fn config(&self) -> anyhow::Result<Option<String>> {
            Ok(self.main.map(str::to_string))
        }

        fn local(&self) -> Option<(std::path::PathBuf, String)> {
            self.local.map(|text| (std::path::PathBuf::from("/proj/.bi.toml"), text.to_string()))
        }
    }

    #[test]
    fn a_local_config_overrides_only_what_it_mentions() {
        let mut ed = Editor::empty();
        let problems = ed.load_config(TwoLayers {
            main: Some("[options]\nnumber = 5\ntab_width = 2\n"),
            local: Some("[options]\ntab_width = 8\n"),
        });
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(ed.session.options.tab_width, 8, "the project said so");
        assert_eq!(ed.session.options.number, LineNumbers::Every(5), "the project said nothing");
    }

    #[test]
    fn a_local_config_works_with_no_main_config_at_all() {
        let mut ed = Editor::empty();
        let problems =
            ed.load_config(TwoLayers { main: None, local: Some("[options]\nnumber = -1\n") });
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(ed.session.options.number, LineNumbers::Relative);
    }

    #[test]
    fn a_local_configs_mistakes_are_reported_with_its_path() {
        let mut ed = Editor::empty();
        let problems = ed.load_config(TwoLayers {
            main: Some("[options]\nnumber = 5\n"),
            local: Some("[options]\nnmber = 9\n"),
        });
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("/proj/.bi.toml"), "{}", problems[0].message);
        assert!(problems[0].message.contains("nmber"), "{}", problems[0].message);
        assert_eq!(ed.session.options.number, LineNumbers::Every(5), "the good layer applied");
    }

    #[test]
    fn an_unparseable_local_config_reports_and_changes_nothing() {
        let mut ed = Editor::empty();
        let problems = ed.load_config(TwoLayers {
            main: Some("[options]\nnumber = 5\n"),
            local: Some("[options\nbroken"),
        });
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains(".bi.toml"), "{}", problems[0].message);
        assert_eq!(
            ed.session.options.number,
            LineNumbers::Every(5),
            "the main config stays whole, never half of each"
        );
    }

    /// The two refusals that make a project config safe to read at all —
    /// see `docs/specs/local-config.md`.
    #[test]
    fn a_local_config_cannot_name_a_binary_or_a_key() {
        let mut ed = Editor::empty();
        let problems = ed.load_config(TwoLayers {
            main: None,
            local: Some(
                "[keys.normal]\n\"j\" = \"left\"\n\n\
                 [lsp.servers.rust-analyzer]\ncommand = [\"evil\"]\nroots = [\"rust-project.json\"]\n",
            ),
        });
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems.iter().any(|p| p.message.contains("keys are not read")), "{problems:?}");
        assert!(problems.iter().any(|p| p.message.contains("command is not read")), "{problems:?}");
        let ra = &ed.config().lsp.servers["rust-analyzer"];
        assert_eq!(ra.command, ["rust-analyzer"], "the built-in command survives");
        assert_eq!(ra.roots, ["rust-project.json"], "the harmless field is read");
        assert!(ed.config().keys.is_empty(), "no binding landed");
    }

    #[test]
    fn load_config_applies_options_and_reports_problems() {
        let mut ed = Editor::empty();
        assert_eq!(ed.session.options.number, LineNumbers::Every(1), "defaults before");

        let problems = ed.load_config(ConfigText(Some("[options]\nnumber = 5\nnmber = 9\n")));

        assert_eq!(ed.session.options.number, LineNumbers::Every(5), "the good line applied");
        assert_eq!(problems.len(), 1, "and the bad one was reported, not fatal");
        assert_eq!(problems[0].line, 3);
    }

    #[test]
    fn no_config_file_is_not_a_problem() {
        // Starting from an editor that already holds the defaults would pass
        // whether or not a missing file actually applies them — load a
        // non-default config first, so this test can tell the difference.
        let mut ed = Editor::empty();
        ed.load_config(ConfigText(Some("[options]\nnumber = 5\n")));
        assert_eq!(ed.session.options.number, LineNumbers::Every(5), "non-default before");

        let problems = ed.load_config(ConfigText(None));
        assert!(problems.is_empty());
        assert_eq!(
            ed.session.options,
            crate::config::Options::default(),
            "no file reverts to defaults, the same as a fresh bi would show"
        );
    }

    #[test]
    fn malformed_config_keeps_what_was_loaded_and_reports_once() {
        let mut ed = Editor::empty();
        ed.load_config(ConfigText(Some("[options]\nnumber = 5\n")));
        assert_eq!(ed.session.options.number, LineNumbers::Every(5));

        // The point of the test: not "ends up default", but "left alone". If
        // `read_config` applied anything on a parse error, this would be
        // `Every(1)` and the test would say so.
        let problems = ed.load_config(ConfigText(Some("[options\nnumber = 5\n")));

        assert_eq!(problems.len(), 1);
        assert_eq!(ed.session.options.number, LineNumbers::Every(5), "the running config survives");
    }

    /// A source whose text can change between reads, which is what `:reload`
    /// is for. `RefCell` rather than a field because `ConfigSource::config`
    /// takes `&self` — the source is read-only to the editor, and mutable
    /// only to its owner. `Option` so a test can also make the file go away
    /// between reloads, the real-world shape of finding 1: a file present at
    /// startup can be gone by the time `:reload` runs.
    struct Mutable(std::cell::RefCell<Option<String>>);

    impl Mutable {
        fn new(text: &str) -> Self {
            Self(std::cell::RefCell::new(Some(text.to_string())))
        }
    }

    impl crate::config::ConfigSource for Mutable {
        fn config(&self) -> anyhow::Result<Option<String>> {
            Ok(self.0.borrow().clone())
        }
    }

    #[test]
    fn reload_picks_up_a_changed_file() {
        let mut ed = Editor::empty();
        let source = std::rc::Rc::new(Mutable::new("[options]\nnumber = 5\n"));
        ed.load_config(std::rc::Rc::clone(&source));
        assert_eq!(ed.session.options.number, LineNumbers::Every(5));

        *source.0.borrow_mut() = Some("[options]\nnumber = -1\n".to_string());
        ex(&mut ed, "reload");

        assert_eq!(ed.session.options.number, LineNumbers::Relative);
        assert_eq!(ed.session.status, "config reloaded");
    }

    #[test]
    fn a_failed_reload_changes_nothing() {
        let mut ed = Editor::empty();
        let source = std::rc::Rc::new(Mutable::new("[options]\nnumber = 5\n"));
        ed.load_config(std::rc::Rc::clone(&source));

        *source.0.borrow_mut() = Some("[options\nnumber = -1\n".to_string());
        ex(&mut ed, "reload");

        assert_eq!(ed.session.options.number, LineNumbers::Every(5), "the running config survives");
        assert!(ed.session.status.contains("config not reloaded"), "{}", ed.session.status);
    }

    #[test]
    fn reload_counts_the_problems_it_kept_going_past() {
        let mut ed = Editor::empty();
        let source = std::rc::Rc::new(Mutable::new("[options]\nnmber = 9\nzz = 1\n"));
        ed.load_config(std::rc::Rc::clone(&source));

        ex(&mut ed, "reload");
        assert_eq!(ed.session.status, "config reloaded — 2 problems");
    }

    #[test]
    fn reload_with_the_config_file_gone_reverts_to_defaults() {
        // The whole-file-absent branch of `read_config` is the one place
        // applied state depends on what was applied before: removing a line
        // from the file reverts correctly because the base is always
        // `Config::default()`, but the file disappearing entirely used to
        // return early and leave the previous options running.
        let mut ed = Editor::empty();
        let source = std::rc::Rc::new(Mutable::new("[options]\nnumber = 5\n"));
        ed.load_config(std::rc::Rc::clone(&source));
        assert_eq!(ed.session.options.number, LineNumbers::Every(5));

        *source.0.borrow_mut() = None;
        ex(&mut ed, "reload");

        assert_eq!(
            ed.session.options,
            crate::config::Options::default(),
            "a fresh bi with no config file would show the defaults"
        );
        assert_eq!(ed.session.status, "config reloaded");
    }

    #[test]
    fn reload_without_a_source_says_so() {
        let mut ed = Editor::empty();
        ex(&mut ed, "reload");
        assert_eq!(ed.session.status, "no config to reload");
    }

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

    /// A bracketed paste is one action however long it is. Before it existed
    /// the terminal sent a paste as keystrokes, and every character cost its
    /// own undo entry, its own reparse and a full redraw — which is what made
    /// pasting look like a typewriter.
    #[test]
    fn a_bracketed_paste_is_one_insertion_and_one_undo_step() {
        let mut ed = editor("");
        ed.apply(cmd(Action::EnterInsert));

        ed.paste_text("one\ntwo\nthree".into());
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\ntwo\nthree");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "", "the whole paste, not a letter");
    }

    /// A paste is typed text, so it goes where typed text goes. In the modes
    /// that take none it is refused rather than invented into a put — `"+p` is
    /// the command that does that, and it says so.
    #[test]
    fn a_paste_reaches_the_command_line_and_is_refused_in_normal_mode() {
        let mut ed = editor("hello");
        ed.apply(cmd(Action::EnterCommandMode));

        ed.paste_text("w out.txt\n".into());

        assert_eq!(ed.session.mode, Mode::Command("w out.txt".into()), "the newline is not typed");

        ed.apply(cmd(Action::CommandCancel));
        ed.paste_text("junk".into());
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "hello", "nothing went in");
        assert!(ed.session.status.contains("\"+p"), "{}", ed.session.status);
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
        ed.apply(operate(
            Operator::Delete,
            Motion::Word { big: false, forward: true, end: false },
            2,
        ));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "baz");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "foo bar baz", "both words, one undo");
    }

    #[test]
    fn c_enters_insert_mode_so_you_can_type_the_replacement() {
        let mut ed = editor("foo bar");
        ed.apply(operate(
            Operator::Change,
            Motion::Word { big: false, forward: true, end: false },
            1,
        ));
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
        ed.apply(operate(
            Operator::Change,
            Motion::Word { big: false, forward: true, end: false },
            1,
        ));
        type_str(&mut ed, "xyz");
        ed.apply(cmd(Action::EnterNormal));

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "foo bar");
    }

    #[test]
    fn a_delete_that_matches_nothing_leaves_no_undo_step() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(
            Operator::Delete,
            Motion::Word { big: false, forward: false, end: false },
            1,
        ));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "bc", "b at char 0 did nothing");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abc", "one undo still reaches the x");
    }

    fn paste(before: bool, count: usize) -> Command {
        cmd(Action::Paste { before, count, sink: Sink::Ring })
    }

    /// A clipboard that stays in the process, which is the whole reason the
    /// core takes a trait: `"+y` and `"+p` are testable without a terminal,
    /// and the escape sequence is tested where it is built.
    #[derive(Default, Clone)]
    struct FakeClipboard(std::rc::Rc<std::cell::RefCell<Option<String>>>);

    impl crate::clipboard::SystemClipboard for FakeClipboard {
        fn set(&self, text: &str) -> anyhow::Result<()> {
            *self.0.borrow_mut() = Some(text.to_string());
            Ok(())
        }
        fn get(&self) -> anyhow::Result<Option<String>> {
            Ok(self.0.borrow().clone())
        }
    }

    #[test]
    fn the_system_register_yanks_out_and_pastes_back_without_touching_the_ring() {
        let clipboard = FakeClipboard::default();
        let mut ed = editor("foo bar");
        ed.set_clipboard(clipboard.clone());

        ed.apply(cmd(Action::Operate {
            op: Operator::Yank,
            target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
            count: 1,
            sink: Sink::System,
        }));

        assert_eq!(clipboard.0.borrow().as_deref(), Some("foo "), "it went outside");
        assert!(ed.session.registers.front().is_none(), "and not into the ring");

        // And back in, from what another program could have put there.
        *clipboard.0.borrow_mut() = Some("zzz".into());
        ed.apply(cmd(Action::Paste { before: true, count: 1, sink: Sink::System }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "zzzfoo bar");
    }

    /// No clipboard is the ordinary state for an embedder that has not supplied
    /// one. It reports; it does not panic, and it does not quietly succeed.
    #[test]
    fn the_system_register_says_so_when_there_is_no_clipboard() {
        let mut ed = editor("foo");

        ed.apply(cmd(Action::Paste { before: false, count: 1, sink: Sink::System }));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "foo", "nothing pasted");
        assert_eq!(ed.session.status, "no system clipboard");
    }

    #[test]
    fn a_clipboard_ending_in_a_newline_comes_back_linewise() {
        let clipboard = FakeClipboard::default();
        *clipboard.0.borrow_mut() = Some("added\n".into());
        let mut ed = editor("first\nsecond");
        ed.set_clipboard(clipboard);

        ed.apply(cmd(Action::Paste { before: false, count: 1, sink: Sink::System }));

        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "first\nadded\nsecond",
            "a whole line, not spliced into the middle of one"
        );
    }

    #[test]
    fn yank_then_paste_round_trips() {
        let mut ed = editor("foo bar");
        ed.apply(operate(
            Operator::Yank,
            Motion::Word { big: false, forward: true, end: false },
            1,
        ));
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
            let path = std::env::temp_dir().join(format!("bi-test-{}-{name}", std::process::id()));
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

    /// Runs a `:` line the way a keystroke does.
    ///
    /// Through `EnterCommandMode` and `CommandExecute` rather than straight
    /// into `run_ex`, because the two are not the same path: `CommandExecute`
    /// takes the mode out of the session before dispatching, and a helper that
    /// skipped it tested a route no key can reach. That is how `:case` over a
    /// rectangle came to act on whole lines with two passing tests over it.
    fn ex(ed: &mut Editor, line: &str) {
        ed.apply(cmd(Action::EnterCommandMode));
        if let Mode::Command(prefilled) = &mut ed.session.mode {
            *prefilled = CmdLine::default();
        }
        for c in line.chars() {
            ed.apply(cmd(Action::CommandChar(c)));
        }
        ed.apply(cmd(Action::CommandExecute));
    }

    /// Out of the box, before any config is loaded.
    #[test]
    fn the_default_theme_is_main() {
        let ed = Editor::empty();
        assert_eq!(ed.session.options.theme, crate::theme::DEFAULT_THEME);
        assert_eq!(ed.theme(), &main_on_screen());
        assert!(ed.theme().ui.background.is_some(), "main claims the background");
    }

    /// `main` as an editor actually shows it, which is not the same thing as
    /// `main` as its file writes it: `italics` is off by default, so the slant
    /// on `comment` and `context` does not survive resolution. See
    /// `docs/specs/theme.md`.
    fn main_on_screen() -> Theme {
        let mut theme = Theme::default();
        theme.drop_italics();
        theme
    }

    /// The option's own test, and the reason the helper above exists: a theme
    /// file keeps its italics, an editor with `italics` off does not show
    /// them, and `:set italics true` gets them back without the name of the
    /// theme having moved.
    #[test]
    fn italics_are_off_by_default_and_set_puts_them_back() {
        let faithful = Theme::default();
        assert!(faithful.style("comment").unwrap().italic, "main.toml still says italic");

        let mut ed = Editor::empty();
        assert!(!ed.session.options.italics, "off by default — see docs/specs/theme.md");
        assert!(!ed.theme().style("comment").unwrap().italic, "the slant reached the screen");

        // A `:set` that reports success and changes nothing on screen is the
        // failure this re-resolve exists to prevent — the same one `:set
        // theme` has.
        ex(&mut ed, "set italics true");
        assert_eq!(ed.session.status, "italics=true");
        assert!(ed.theme().style("comment").unwrap().italic, "italics did not come back");
        assert_eq!(ed.theme(), &faithful, "and nothing else moved with them");

        ex(&mut ed, "set italics false");
        assert!(!ed.theme().style("comment").unwrap().italic);
    }

    /// A name is not a palette. `:set theme ansi` that moved the string and
    /// left `self.theme` alone would report success and change nothing on
    /// screen, which is the one failure worth engineering against here.
    #[test]
    fn setting_the_theme_resolves_it_rather_than_just_naming_it() {
        let mut ed = Editor::empty();
        let before = ed.theme().clone();

        ex(&mut ed, "set theme ansi");
        assert_eq!(ed.session.options.theme, "ansi");
        assert_ne!(ed.theme(), &before, "the palette did not follow the name");
        // The whole point of the ANSI spelling: the terminal's own background.
        assert_eq!(ed.theme().ui.background, None);
        assert_eq!(ed.session.status, "theme=ansi");

        ex(&mut ed, "set theme main");
        assert_eq!(ed.theme(), &before, "and back again");
    }

    /// The whole point is that a window editing files on another machine does
    /// not look like one that is not, so the two must actually differ.
    #[test]
    fn a_remote_session_takes_the_ssh_theme() {
        let mut ed = Editor::empty();
        assert!(!ed.is_remote());
        let local = ed.theme().clone();

        ed.set_remote(true);
        assert!(ed.is_remote());
        assert_ne!(ed.theme(), &local, "a remote session looked identical");
        // The default pairing: dark locally, light over the wire.
        assert_eq!(ed.session.options.ssh_theme, "gruvbox-light");
        assert_eq!(ed.theme().ui.background, Some(crate::theme::Color::Rgb(0xfb, 0xf1, 0xc7)));

        ed.set_remote(false);
        assert_eq!(ed.theme(), &local, "and back again");
    }

    /// `set_remote` re-resolves, so a frontend may call it before or after
    /// `load_config` — which matters because `main.rs` has to pick one.
    #[test]
    fn the_ssh_theme_lands_whichever_order_the_frontend_uses() {
        let config = "[options]\nssh_theme = \"pascal\"\n";

        let mut before = Editor::empty();
        before.set_remote(true);
        before.load_config(ConfigText(Some(config)));

        let mut after = Editor::empty();
        after.load_config(ConfigText(Some(config)));
        after.set_remote(true);

        assert_eq!(before.theme(), after.theme());
        assert_eq!(before.theme().ui.background, Some(crate::theme::Color::Rgb(0, 0, 0xa8)));
    }

    /// Over SSH the name in force is `ssh_theme`, so that is the one `:set`
    /// has to move — changing `theme` there would report success and leave the
    /// screen alone.
    #[test]
    fn set_moves_whichever_theme_name_is_in_force() {
        let mut ed = Editor::empty();
        ed.set_remote(true);
        let light = ed.theme().clone();

        ex(&mut ed, "set theme ansi");
        assert_eq!(ed.theme(), &light, "the local theme is not the live one here");

        ex(&mut ed, "set ssh_theme pascal");
        assert_eq!(ed.session.status, "ssh_theme=pascal");
        assert_eq!(ed.theme().ui.background, Some(crate::theme::Color::Rgb(0, 0, 0xa8)));
    }

    #[test]
    fn an_unknown_theme_name_says_so_and_keeps_an_editor() {
        let mut ed = Editor::empty();
        ex(&mut ed, "set theme nosuch");
        assert!(ed.session.status.contains("nosuch"), "{}", ed.session.status);
        // Fell back rather than leaving the screen colourless.
        assert_eq!(ed.theme(), &main_on_screen());
    }

    #[test]
    fn a_config_file_can_name_the_theme() {
        let mut ed = Editor::empty();
        let problems = ed.load_config(ConfigText(Some("[options]\ntheme = \"ansi\"\n")));
        assert_eq!(problems, []);
        assert_eq!(ed.theme().ui.background, None, "ansi leaves the terminal's alone");
    }

    /// `:themes` lists exactly `Theme::builtins()` — no alias among the rows,
    /// which is the check `an_alias_reaches_its_theme_and_is_not_itself_listed`
    /// makes from the theme side, read from the command side.
    #[test]
    fn themes_lists_every_builtin_and_no_alias() {
        let mut ed = Editor::empty();
        ex(&mut ed, "themes");
        let picker = ed.session.picker.as_ref().expect("themes opens a picker");
        assert_eq!(picker.kind, PickerKind::Theme);

        let names: Vec<&str> = picker.items().iter().map(|i| i.text.as_str()).collect();
        assert_eq!(names, crate::theme::Theme::builtins().collect::<Vec<_>>());
        for alias in ["gameboy", "forest", "molokai", "nord", "gruvbox-dark"] {
            assert!(!names.contains(&alias), "`{alias}` is an alias and should not be a row");
        }
    }

    /// It opens oriented on where you already are, not at the front of the
    /// list — a browse rather than a toggle, unlike the buffer switcher.
    #[test]
    fn themes_opens_on_the_current_theme_and_marks_its_row() {
        let mut ed = Editor::empty();
        ex(&mut ed, "set theme vesper");
        ex(&mut ed, "themes");

        let picker = ed.session.picker.as_ref().unwrap();
        let at = picker.selected().expect("a default row is selected");
        assert_eq!(picker.items()[at].text, "vesper");
        assert_eq!(picker.items()[at].badge, Some('✓'), "the current theme is marked");
        assert_eq!(
            picker.items().iter().filter(|i| i.badge.is_some()).count(),
            1,
            "only one row is current"
        );
    }

    /// Accepting a row is `:set theme <name>`, not a shadow of it — same
    /// resolved theme, same status line, same option moved.
    #[test]
    fn accepting_a_theme_is_indistinguishable_from_set_theme() {
        let mut direct = Editor::empty();
        ex(&mut direct, "set theme monokai");

        let mut picked = Editor::empty();
        ex(&mut picked, "themes");
        let at = picked
            .session
            .picker
            .as_ref()
            .unwrap()
            .items()
            .iter()
            .position(|i| i.text == "monokai")
            .expect("monokai is a row");
        for _ in 0..at {
            pick_keys(&mut picked, &[Action::PickNext]);
        }
        pick_keys(&mut picked, &[Action::PickAccept]);

        assert!(picked.session.picker.is_none());
        assert_eq!(picked.session.mode, Mode::Normal);
        assert_eq!(picked.theme(), direct.theme());
        assert_eq!(picked.session.status, direct.session.status);
        assert_eq!(picked.session.options.theme, "monokai");
    }

    /// Typing narrows it the way every other subsequence picker does.
    #[test]
    fn typing_filters_the_theme_list_by_subsequence() {
        let mut ed = Editor::empty();
        ex(&mut ed, "themes");
        pick_keys(&mut ed, &[Action::PickChar('n'), Action::PickChar('r'), Action::PickChar('d')]);
        let picker = ed.session.picker.as_ref().unwrap();
        let names: Vec<&str> =
            picker.matches().iter().map(|&i| picker.items()[i].text.as_str()).collect();
        assert!(names.contains(&"nordark"), "{names:?}");
        assert!(!names.contains(&"vesper"), "{names:?}");
    }

    #[test]
    fn a_theme_that_is_not_a_string_is_reported_and_not_fatal() {
        let mut ed = Editor::empty();
        let problems = ed.load_config(ConfigText(Some("[options]\ntheme = 7\n")));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.contains("theme"), "{:?}", problems[0].message);
        assert_eq!(ed.theme(), &main_on_screen());
    }

    /// A directory under the temp dir, gone when the test ends.
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("bi-dir-{}-{name}", std::process::id()));
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

        fn written(self, rel: &str, text: &str) -> Self {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
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

    /// `bi .` — and the buffer list stays non-empty, so nothing downstream
    /// has to learn that the session began on a directory.
    #[test]
    fn opening_a_directory_starts_on_a_tree() {
        let d = ScratchDir::new("open").file("a.rs");

        let ed = Editor::open(d.path()).unwrap();

        assert!(ed.window().tree().is_some());
        assert_eq!(ed.buffer_ids().len(), 1, "with a [No Name] behind it, until a file arrives");
    }

    /// The `[No Name]` `bi .` leaves behind is a placeholder for the invariant,
    /// not a buffer anyone asked for. Opening a file from the tree collects it,
    /// or `Ctrl-I` would cycle between the file and a blank forever.
    #[test]
    fn the_placeholder_buffer_goes_when_a_real_file_arrives() {
        let d = ScratchDir::new("sweep-tree").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);

        tree_key(&mut ed, TreeCmd::Enter);

        assert_eq!(ed.buffer_ids().len(), 1, "the file, and nothing behind it");
        assert!(ed.buffer().and_then(|b| b.path.as_ref()).is_some(), "and it is the file");
    }

    /// The same wart by the other road: plain `bi`, then `:e`. This is how vim
    /// reuses its initial blank.
    #[test]
    fn the_initial_blank_goes_when_it_is_edited_away_from() {
        let d = ScratchDir::new("sweep-edit").file("a.rs");
        let mut ed = Editor::empty();

        ex(&mut ed, &format!("e {}/a.rs", d.path()));

        assert_eq!(ed.buffer_ids().len(), 1);
        assert!(ed.window().alt_buffer().is_none(), "and no Ctrl-^ back to a blank");
    }

    /// The three conditions are each load-bearing. A blank you are looking at
    /// stays; one holding text stays; one that is modified stays even when the
    /// text has been deleted back to nothing.
    #[test]
    fn a_scratch_buffer_that_is_in_use_is_not_swept() {
        let d = ScratchDir::new("sweep-keep").file("a.rs");
        let mut ed = editor("hello");
        sized(&mut ed);
        ex(&mut ed, &format!("sp {}/a.rs", d.path()));

        assert_eq!(ed.buffer_ids().len(), 2, "the unnamed buffer is still on screen below");

        // Emptied by editing is not the same as never written in: the undo
        // history is a reason to keep it.
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::CurrentLine),
            count: 1,
            sink: Sink::Ring,
        }));
        ex(&mut ed, "close");

        assert_eq!(ed.buffer_ids().len(), 2, "modified, so it survives having no window");
    }

    /// `:new` is not `:split`. It shipped as an alias, which gave anyone who
    /// typed it a second view of the file they already had and said nothing.
    #[test]
    fn the_new_commands_open_an_unnamed_buffer_rather_than_this_one() {
        let mut ed = editor("hello");
        sized(&mut ed);

        ex(&mut ed, "new");

        assert_eq!(ed.window_ids().len(), 2, "a split");
        assert_eq!(ed.buffer_ids().len(), 2, "onto a buffer of its own");
        assert!(ed.buffer().unwrap().path.is_none(), "unnamed");
        assert_eq!(ed.buffer().unwrap().rope().len_chars(), 0, "and empty");

        // `:enew` is the same buffer without the split.
        let windows = ed.window_ids().len();
        ex(&mut ed, "enew");
        assert_eq!(ed.window_ids().len(), windows, "this window");
        assert!(ed.buffer().unwrap().path.is_none());
    }

    #[test]
    fn vnew_splits_the_other_way() {
        let mut ed = editor("hello");
        sized(&mut ed);

        ex(&mut ed, "vnew");

        let rects: Vec<_> = ed
            .window_ids()
            .into_iter()
            .filter_map(|id| ed.layout.rect_of(id, ed.area, &ed.chrome))
            .collect();
        assert_eq!(rects.len(), 2);
        assert_ne!(rects[0].x, rects[1].x, "side by side, not stacked");
        assert!(ed.buffer().unwrap().path.is_none());
    }

    /// The spellings that were already right stay right: a bare `:sp` is a
    /// second view of this buffer, not a new one.
    #[test]
    fn a_bare_split_still_duplicates_the_window() {
        let mut ed = editor("hello");
        sized(&mut ed);

        ex(&mut ed, "sp");

        assert_eq!(ed.window_ids().len(), 2);
        assert_eq!(ed.buffer_ids().len(), 1);
    }

    /// Every way of making a window moves focus into it. One test over all of
    /// them, because the promise is the spec's and the arms that keep it are
    /// spread across `run_window_cmd`.
    #[test]
    fn focus_follows_every_way_of_making_a_window() {
        let d = ScratchDir::new("probe").file("a.rs");
        for line in ["sp", "vs", "new", "vnew"] {
            let mut ed = editor("hello");
            sized(&mut ed);
            let before = ed.focus();
            ex(&mut ed, line);
            assert_ne!(ed.focus(), before, ":{line} did not move focus");
        }
        // With a path.
        let mut ed = editor("hello");
        sized(&mut ed);
        let before = ed.focus();
        ex(&mut ed, &format!("sp {}/a.rs", d.path()));
        assert_ne!(ed.focus(), before, ":sp <path> did not move focus");

        // Ctrl-W s / v / e.
        for cmd_ in [
            WindowCmd::Split { dir: Dir::Horizontal, path: None },
            WindowCmd::Split { dir: Dir::Vertical, path: None },
            WindowCmd::Tree,
        ] {
            let mut ed = editor("hello");
            sized(&mut ed);
            let before = ed.focus();
            ed.apply(cmd(Action::Window(cmd_.clone())));
            assert_ne!(ed.focus(), before, "{cmd_:?} did not move focus");
        }
    }

    /// A split opens *beside* what you were reading, not on top of the space
    /// it occupied. Focus followed the new window before this too, but the new
    /// window took the old one's place on screen, so moving into it looked
    /// exactly like not moving at all.
    #[test]
    fn a_split_opens_on_the_far_side_and_focus_is_there() {
        let mut ed = editor("hello");
        sized(&mut ed);
        let before = ed.focus();

        ex(&mut ed, "vs");

        let rect = |ed: &Editor, id| ed.layout.rect_of(id, ed.area, &ed.chrome).unwrap();
        assert_ne!(ed.focus(), before, "focus moved");
        assert!(rect(&ed, ed.focus()).x > rect(&ed, before).x, "and it moved to the right");

        // Horizontally the same, downwards.
        let mut ed = editor("hello");
        sized(&mut ed);
        let before = ed.focus();
        ex(&mut ed, "sp");
        assert!(rect(&ed, ed.focus()).y > rect(&ed, before).y, "below, not above");
    }

    /// The sidebar is the exception: a file tree belongs on the left whichever
    /// side a plain split opens on.
    #[test]
    fn the_tree_sidebar_still_opens_on_the_left() {
        let d = ScratchDir::new("sidebar").file("a.rs");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        sized(&mut ed);
        let before = ed.focus();

        ed.apply(cmd(Action::Window(WindowCmd::Tree)));

        let rect = |ed: &Editor, id| ed.layout.rect_of(id, ed.area, &ed.chrome).unwrap();
        assert!(ed.window().tree().is_some(), "focus is in the tree");
        assert!(rect(&ed, ed.focus()).x < rect(&ed, before).x, "and the tree is on the left");
    }

    /// And on the left of the *screen*. Splitting the focused pane put the
    /// tree wherever you happened to be standing — open it from the bottom
    /// half of a `:sp` and it was a half-height column in the bottom left,
    /// which is not a sidebar. See `docs/specs/tree.md`.
    #[test]
    fn the_tree_is_a_column_of_the_screen_whatever_the_splits_are() {
        let d = ScratchDir::new("sidebar-full").file("a.rs");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        sized(&mut ed);
        let top = ed.focus();
        ex(&mut ed, "sp");
        let bottom = ed.focus();
        assert_ne!(top, bottom, "two panes, stacked");

        ed.apply(cmd(Action::Window(WindowCmd::Tree)));

        let rect = |ed: &Editor, id| ed.layout.rect_of(id, ed.area, &ed.chrome).unwrap();
        let tree = rect(&ed, ed.focus());
        assert!(tree.x < rect(&ed, top).x, "left of the pane above");
        assert!(tree.x < rect(&ed, bottom).x, "and of the one below");
        assert_eq!((tree.y, tree.height), (ed.area.y, ed.area.height), "full height");
    }

    /// `:vs .` is the same sidebar by another name, so it goes to the same
    /// place at the same width — and `:sp .` too, because a directory asked
    /// for a tree and a tree belongs on the left whichever way you spelled the
    /// split.
    #[test]
    fn naming_a_directory_in_a_split_opens_the_sidebar_too() {
        let d = ScratchDir::new("sidebar-named").file("a.rs");
        for line in ["vs", "sp"] {
            let mut ed = editor("one");
            sized(&mut ed);
            let before = ed.focus();
            ex(&mut ed, &format!("{line} {}", d.path()));

            let rect = |ed: &Editor, id| ed.layout.rect_of(id, ed.area, &ed.chrome).unwrap();
            let tree = rect(&ed, ed.focus());
            assert!(ed.window().tree().is_some(), "{line} opened a tree");
            assert!(tree.x < rect(&ed, before).x, "{line} put it on the left");
            assert_eq!((tree.y, tree.height), (ed.area.y, ed.area.height), "{line}: full height");
            assert!(
                tree.width <= TEST_CHROME.tree_width + 1,
                "{line}: a sidebar, not half the screen: {}",
                tree.width
            );
        }
    }

    /// The whole point of an ex binding: `:bd` was not reachable by any name,
    /// because a name has to be something bi already has keys for.
    #[test]
    fn a_leader_binding_runs_an_ex_line() {
        let d = ScratchDir::new("ex-bind").file("a.rs").file("b.rs");
        let (config, problems) = crate::config::parse(
            "[keys.normal]\n\"<leader>d\" = \":bd<CR>\"\n\"<leader>n\" = \":set number 0<CR>\"\n\"<leader>e\" = \":e \"\n",
            crate::config::Config::default(),
        )
        .expect("parses");
        assert!(problems.is_empty(), "{problems:?}");

        let mut input = crate::input::Input::default();
        input.set_keys(config.keys);
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        sized(&mut ed);
        ex(&mut ed, &format!("e {}/b.rs", d.path()));
        assert_eq!(ed.buffer_ids().len(), 2);

        let press = |ed: &mut Editor, input: &mut crate::input::Input, c: char| {
            let key = crate::key::Key::char(c);
            if let Some(command) = input.on_key(key, &ed.session.mode, ed.content_kind()) {
                ed.apply(command);
            }
        };

        press(&mut ed, &mut input, ' ');
        press(&mut ed, &mut input, 'd');
        assert_eq!(ed.buffer_ids().len(), 1, "the buffer was deleted");
        assert!(ed.session.status.contains("deleted"), "{}", ed.session.status);

        // An ex line with an argument, and one that is not about buffers.
        press(&mut ed, &mut input, ' ');
        press(&mut ed, &mut input, 'n');
        assert_eq!(ed.session.options.number, LineNumbers::Off);

        // The count does not repeat it: `3<leader>n` is one `:set`, not three.
        press(&mut ed, &mut input, '3');
        press(&mut ed, &mut input, ' ');
        press(&mut ed, &mut input, 'n');
        assert_eq!(ed.session.options.number, LineNumbers::Off, "still just set once");

        // Without a `<CR>` the line is prefilled and waits, which is how a
        // binding asks for an argument.
        press(&mut ed, &mut input, ' ');
        press(&mut ed, &mut input, 'e');
        assert_eq!(ed.session.mode, Mode::Command("e ".into()), "left for you to finish");
    }

    /// A leader binding for a window command has to work from inside the tree,
    /// or the key that opens the sidebar cannot close it — which is exactly
    /// what happened. The tree keymap is an allowlist and borrows no single
    /// key from `[keys.normal]`, but a sequence is a command the user invented
    /// and collides with nothing the tree binds.
    #[test]
    fn a_leader_binding_toggles_the_tree_from_either_side() {
        let d = ScratchDir::new("leader-tree").file("a.rs").file("b.rs");
        let (config, problems) = crate::config::parse(
            "[keys.normal]\n\"<leader>e\" = \"window_tree\"\n\"j\" = \"left\"\n",
            crate::config::Config::default(),
        )
        .expect("parses");
        assert!(problems.is_empty(), "{problems:?}");

        let mut input = crate::input::Input::default();
        input.set_keys(config.keys);
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        sized(&mut ed);

        let press = |ed: &mut Editor, input: &mut crate::input::Input, c: char| {
            let key = crate::key::Key::char(c);
            if let Some(command) = input.on_key(key, &ed.session.mode, ed.content_kind()) {
                ed.apply(command);
            }
        };

        press(&mut ed, &mut input, ' ');
        press(&mut ed, &mut input, 'e');
        assert_eq!(ed.window_ids().len(), 2, "the tree opened");
        assert!(ed.window().tree().is_some(), "and took focus");

        press(&mut ed, &mut input, ' ');
        press(&mut ed, &mut input, 'e');
        assert_eq!(ed.window_ids().len(), 1, "and the same keys put it away");

        // What the tree's own keymap still refuses to borrow: a key it has a
        // meaning for. `j` must select down here, not collapse.
        press(&mut ed, &mut input, ' ');
        press(&mut ed, &mut input, 'e');
        let before = ed.window().tree().map(|t| t.selected());
        press(&mut ed, &mut input, 'j');
        assert_ne!(ed.window().tree().map(|t| t.selected()), before, "j still moves down");
    }

    /// The same toggle as one key. A tree borrows what it has no meaning for,
    /// and `<C-b>` — the key most people reach for — is not in its vocabulary,
    /// so it has to work from both sides exactly as `<leader>e` does.
    #[test]
    fn a_single_key_binding_toggles_the_tree_from_either_side() {
        let d = ScratchDir::new("ctrl-b-tree").file("a.rs").file("b.rs");
        let (config, problems) = crate::config::parse(
            "[keys.normal]\n\"<C-b>\" = \"window_tree\"\n",
            crate::config::Config::default(),
        )
        .expect("parses");
        assert!(problems.is_empty(), "{problems:?}");

        let mut input = crate::input::Input::default();
        input.set_keys(config.keys);
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        sized(&mut ed);

        let mut press = |ed: &mut Editor| {
            let key = crate::key::Key::ctrl('b');
            if let Some(command) = input.on_key(key, &ed.session.mode, ed.content_kind()) {
                ed.apply(command);
            }
        };

        press(&mut ed);
        assert_eq!(ed.window_ids().len(), 2, "the tree opened");
        assert!(ed.window().tree().is_some(), "and took focus");

        press(&mut ed);
        assert_eq!(ed.window_ids().len(), 1, "and the same key put it away");
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
    /// where there is no buffer. Without this `:q` cannot leave `bi .`.
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

    /// Types a `:` line and runs it, the way a keystroke would — `ex` calls the
    /// parser directly and so never touches the history.
    fn run_typed(ed: &mut Editor, line: &str) {
        ed.apply(cmd(Action::EnterCommandMode));
        for c in line.chars() {
            ed.apply(cmd(Action::CommandChar(c)));
        }
        ed.apply(cmd(Action::CommandExecute));
    }

    /// Puts a half-typed line on the `:` line and leaves it there.
    fn start_typing(ed: &mut Editor, line: &str) {
        ed.apply(cmd(Action::EnterCommandMode));
        for c in line.chars() {
            ed.apply(cmd(Action::CommandChar(c)));
        }
    }

    fn open_history(ed: &mut Editor) {
        ed.apply(cmd(Action::OpenPicker(PickerKind::History)));
    }

    /// The `:` line as it stands, and where its cursor is.
    fn cmdline(ed: &Editor) -> (String, usize) {
        let Mode::Command(line) = &ed.session.mode else { panic!("not on the `:` line") };
        (line.to_string(), line.cursor())
    }

    fn cmd_move(ed: &mut Editor, how: CmdMove) {
        ed.apply(cmd(Action::CommandMove(how)));
    }

    fn recall(ed: &mut Editor, older: bool) {
        ed.apply(cmd(Action::CommandRecall { older }));
    }

    /// A prompt with no cursor makes you hold Backspace to fix one character.
    /// See `docs/specs/cmdline.md`.
    #[test]
    fn typing_lands_where_the_cursor_is() {
        let mut ed = editor("hello");
        start_typing(&mut ed, "sm/a/b/");
        cmd_move(&mut ed, CmdMove::Home);
        ed.apply(cmd(Action::CommandChar('%')));

        assert_eq!(cmdline(&ed), ("%sm/a/b/".to_string(), 1));
    }

    #[test]
    fn backspace_takes_what_is_before_the_cursor_and_an_empty_line_leaves() {
        let mut ed = editor("hello");
        start_typing(&mut ed, "wq");
        cmd_move(&mut ed, CmdMove::Left);
        ed.apply(cmd(Action::CommandBackspace));
        assert_eq!(cmdline(&ed), ("q".to_string(), 0));

        // At column 0 with text still on the line there is nothing to delete,
        // and nothing to delete is not a reason to leave.
        ed.apply(cmd(Action::CommandBackspace));
        assert_eq!(cmdline(&ed), ("q".to_string(), 0));

        cmd_move(&mut ed, CmdMove::End);
        ed.apply(cmd(Action::CommandBackspace));
        ed.apply(cmd(Action::CommandBackspace));
        assert_eq!(ed.session.mode, Mode::Normal, "an empty line still leaves");
    }

    #[test]
    fn the_ends_of_the_line_are_a_keypress_away() {
        let mut ed = editor("hello");
        start_typing(&mut ed, "set number");

        cmd_move(&mut ed, CmdMove::Home);
        assert_eq!(cmdline(&ed).1, 0);
        cmd_move(&mut ed, CmdMove::Left);
        assert_eq!(cmdline(&ed).1, 0, "and stays there");

        cmd_move(&mut ed, CmdMove::End);
        assert_eq!(cmdline(&ed).1, "set number".len());
        cmd_move(&mut ed, CmdMove::Right);
        assert_eq!(cmdline(&ed).1, "set number".len());
    }

    /// `Up` is for the last thing you ran, or the one before it; `Ctrl-R` is
    /// for finding one. Both walk the same list.
    #[test]
    fn up_and_down_walk_the_history_and_give_the_draft_back() {
        let mut ed = editor("hello");
        run_typed(&mut ed, "set number");
        run_typed(&mut ed, "ls");
        start_typing(&mut ed, "half");

        recall(&mut ed, true);
        assert_eq!(cmdline(&ed), ("ls".to_string(), 2), "newest first, cursor at its end");
        recall(&mut ed, true);
        assert_eq!(cmdline(&ed).0, "set number");
        recall(&mut ed, true);
        assert_eq!(cmdline(&ed).0, "set number", "the oldest does not wrap");

        recall(&mut ed, false);
        assert_eq!(cmdline(&ed).0, "ls");
        recall(&mut ed, false);
        assert_eq!(cmdline(&ed).0, "half", "past the newest is what you were typing");
    }

    #[test]
    fn a_recalled_line_runs_and_is_recorded_again() {
        let mut ed = editor("hello");
        run_typed(&mut ed, "set number 3");
        ed.apply(cmd(Action::EnterCommandMode));
        recall(&mut ed, true);
        ed.apply(cmd(Action::CommandExecute));

        assert_eq!(ed.session.cmd_history.lines(), ["set number 3"], "one entry, not two");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn up_with_no_history_leaves_the_line_alone() {
        let mut ed = editor("hello");
        start_typing(&mut ed, "half");
        recall(&mut ed, true);

        assert_eq!(cmdline(&ed), ("half".to_string(), 4));
    }

    #[test]
    fn running_a_typed_line_records_it_newest_first() {
        let mut ed = editor("hello");

        run_typed(&mut ed, "set number");
        run_typed(&mut ed, "ls");

        assert_eq!(ed.session.cmd_history.lines(), ["ls", "set number"]);
    }

    /// A keybinding or an internal caller reaching the same parser is not
    /// something you typed, and a history of those is noise in the one list
    /// that exists to give your own keystrokes back.
    #[test]
    fn a_line_run_by_a_keybinding_is_not_history() {
        let mut ed = editor("hello");

        ed.apply(cmd(Action::Ex { line: "set number".into(), run: true }));

        assert!(ed.session.cmd_history.is_empty());
    }

    /// The line with a word wrong is the one you most want back.
    #[test]
    fn a_command_that_failed_is_still_in_the_history() {
        let mut ed = editor("hello");

        run_typed(&mut ed, "nosuchcommand");

        assert_eq!(ed.session.cmd_history.lines(), ["nosuchcommand"]);
        assert!(!ed.session.status.is_empty(), "and it still said why");
    }

    #[test]
    fn ctrl_r_opens_the_history_narrowed_to_what_you_had_typed() {
        let mut ed = editor("hello");
        run_typed(&mut ed, "set number");
        run_typed(&mut ed, "ls");
        start_typing(&mut ed, "set");

        open_history(&mut ed);

        let picker = ed.session.picker.as_ref().expect("the picker is up");
        assert_eq!(ed.session.mode, Mode::Pick);
        assert_eq!(picker.query(), "set", "seeded with the half-typed line");
        let shown: Vec<&str> =
            picker.matches().iter().map(|i| picker.items()[*i].text.as_str()).collect();
        assert_eq!(shown, ["set number"], "and narrowed to it");
    }

    /// The whole point: it arrives on the `:` line and waits to be edited.
    #[test]
    fn accepting_a_history_line_types_it_out_and_runs_nothing() {
        let mut ed = editor("hello");
        run_typed(&mut ed, "set number 5");
        ed.session.options.number = LineNumbers::Every(1);
        start_typing(&mut ed, "");

        open_history(&mut ed);
        ed.apply(cmd(Action::PickAccept));

        assert_eq!(ed.session.mode, Mode::Command("set number 5".into()));
        assert_eq!(ed.session.options.number, LineNumbers::Every(1), "not run");
        assert!(ed.session.picker.is_none(), "and the overlay is gone");
    }

    /// Cancelling has to give the line back, or `Ctrl-R` becomes a key you
    /// hesitate over.
    #[test]
    fn cancelling_the_history_gives_the_half_typed_line_back() {
        let mut ed = editor("hello");
        run_typed(&mut ed, "ls");
        start_typing(&mut ed, "w out");

        open_history(&mut ed);
        ed.apply(cmd(Action::PickChar('l')));
        ed.apply(cmd(Action::PickCancel));

        assert_eq!(ed.session.mode, Mode::Command("w out".into()), "exactly as it was");
    }

    #[test]
    fn an_empty_history_says_so_and_leaves_you_on_the_command_line() {
        let mut ed = editor("hello");
        start_typing(&mut ed, "w");

        open_history(&mut ed);

        assert_eq!(ed.session.status, "no command history");
        assert_eq!(ed.session.mode, Mode::Command("w".into()));
        assert!(ed.session.picker.is_none());
    }

    /// One-character commands are the most typed there are. The register
    /// ring's length floor would have hidden every one of them.
    #[test]
    fn the_shortest_commands_are_in_the_list() {
        let mut ed = editor("hello");
        // Refused — there is no file name — and recorded all the same.
        run_typed(&mut ed, "w");
        start_typing(&mut ed, "");

        open_history(&mut ed);

        let picker = ed.session.picker.as_ref().expect("the picker is up");
        let shown: Vec<&str> =
            picker.matches().iter().map(|i| picker.items()[*i].text.as_str()).collect();
        assert_eq!(shown, ["w"], "the register ring's length floor would have hidden it");
    }

    /// The `:` line works in a tree window, and so must the history over it.
    #[test]
    fn the_history_picker_works_from_a_tree_window() {
        let d = ScratchDir::new("tree-history").file("a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        run_typed(&mut ed, "set number");
        start_typing(&mut ed, "");

        open_history(&mut ed);
        ed.apply(cmd(Action::PickAccept));

        assert_eq!(ed.session.mode, Mode::Command("set number".into()));
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

    // ---- `:s` ---------------------------------------------------------------

    fn rope_of(ed: &Editor) -> String {
        ed.buffer().unwrap().rope().to_string()
    }

    /// The line that started this: `:%s/2024/2025/g` used to say
    /// "`:%s/2024/2025/g` takes no range", because there was no `:s` for the
    /// range to belong to. See `docs/specs/substitute.md`.
    #[test]
    fn a_whole_file_substitution_rewrites_every_occurrence() {
        let mut ed = editor("(c) 2024\nbuilt 2024, shipped 2024\n");

        ex(&mut ed, "%s/2024/2025/g");

        assert_eq!(rope_of(&ed), "(c) 2025\nbuilt 2025, shipped 2025\n");
        assert_eq!(ed.session.status, "3 substitutions on 2 lines");
    }

    #[test]
    fn no_range_is_the_cursors_line_and_a_range_is_its_lines() {
        let mut ed = editor("a\na\na\n");
        ed.set_cursor(Cursor::at(2));
        ex(&mut ed, "s/a/b/");
        assert_eq!(rope_of(&ed), "a\nb\na\n", "the line the cursor was on");

        let mut ed = editor("a\na\na\n");
        ex(&mut ed, "2,3s/a/b/");
        assert_eq!(rope_of(&ed), "a\nb\nb\n");
    }

    /// Without `g` it is the first match on each line, which is vim and is why
    /// `g` is typed as often as it is.
    #[test]
    fn without_g_only_the_first_match_on_a_line_goes() {
        let mut ed = editor("a a a\na a\n");

        ex(&mut ed, "%s/a/b/");

        assert_eq!(rope_of(&ed), "b a a\nb a\n");
        assert_eq!(ed.session.status, "2 substitutions on 2 lines");
    }

    /// Applied back to front, so an earlier replacement cannot shift a later
    /// one's offsets — and matching happens before any of them, so a
    /// replacement is never searched for the pattern it just made.
    #[test]
    fn replacements_do_not_move_or_chase_each_other() {
        let mut ed = editor("a-a\n");
        ex(&mut ed, "%s/a/LONGER/g");
        assert_eq!(rope_of(&ed), "LONGER-LONGER\n");

        let mut ed = editor("aaa\n");
        ex(&mut ed, "%s/a/aa/g");
        assert_eq!(rope_of(&ed), "aaaaaa\n", "each `a` doubled once, not forever");
    }

    #[test]
    fn the_whole_command_is_one_undo_step() {
        let mut ed = editor("a\na\na\n");
        ex(&mut ed, "%s/a/b/g");
        assert_eq!(rope_of(&ed), "b\nb\nb\n");

        ed.apply(cmd(Action::Undo));

        assert_eq!(rope_of(&ed), "a\na\na\n", "one `u`, not three");
    }

    /// Smartcase is the default, as it is for `/`; the flags are for when that
    /// guess is wrong.
    #[test]
    fn the_case_flags_override_smartcase_in_both_directions() {
        let mut ed = editor("Foo foo\n");
        ex(&mut ed, "%s/Foo/x/g");
        assert_eq!(rope_of(&ed), "x foo\n", "an uppercase pattern is case-sensitive");

        let mut ed = editor("Foo foo\n");
        ex(&mut ed, "%s/Foo/x/gi");
        assert_eq!(rope_of(&ed), "x x\n", "`i` matches both");

        let mut ed = editor("Foo foo\n");
        ex(&mut ed, "%s/foo/x/gI");
        assert_eq!(rope_of(&ed), "Foo x\n", "`I` matches only the one that is spelled that way");
    }

    #[test]
    fn n_counts_and_changes_nothing() {
        let mut ed = editor("a a\na\n");

        ex(&mut ed, "%s/a/b/gn");

        assert_eq!(rope_of(&ed), "a a\na\n");
        assert_eq!(ed.session.status, "3 substitutions on 2 lines");
    }

    /// A `:s` that quietly does nothing is one you assume worked.
    #[test]
    fn nothing_matched_says_so_and_changes_nothing() {
        let mut ed = editor("alpha\n");

        ex(&mut ed, "%s/zebra/x/g");

        assert_eq!(rope_of(&ed), "alpha\n");
        assert_eq!(ed.session.status, "pattern not found: zebra");
    }

    #[test]
    fn the_cursor_lands_on_the_last_line_changed_and_n_walks_what_it_did() {
        let mut ed = editor("a\nfiller\na\nfiller\n");

        ex(&mut ed, "%s/a/b/g");

        let row = ed.buffer().unwrap().row_at(ed.cursor().unwrap());
        assert_eq!(row, 2, "the last line it touched");
        assert_eq!(
            ed.session.last_search.as_ref().map(|s| s.pattern.as_str()),
            Some("a"),
            "and `n` looks for what was replaced",
        );
    }

    /// `/foo` then `:%s//bar/g` is the pair everyone uses.
    #[test]
    fn an_empty_pattern_is_the_last_search() {
        let mut ed = editor("one two\n");
        ed.session.last_search =
            Some(Search { pattern: "two".into(), whole_word: false, forward: true });

        ex(&mut ed, "%s//2/g");

        assert_eq!(rope_of(&ed), "one 2\n");
    }

    #[test]
    fn a_delimiter_that_is_not_a_slash_keeps_a_path_readable() {
        let mut ed = editor("/usr/local/bin\n");

        ex(&mut ed, "%s#/usr/local#/opt#");

        assert_eq!(rope_of(&ed), "/opt/bin\n");
    }

    /// The range rules are `ranges.md`'s and are not re-implemented here.
    #[test]
    fn a_range_naming_a_line_that_is_not_there_is_refused() {
        let mut ed = editor("a\nb\n");

        ex(&mut ed, "2,99s/a/b/");

        assert_eq!(rope_of(&ed), "a\nb\n");
        assert_eq!(ed.session.status, "no line 99");
    }

    /// `:set` and `:sp` start with an `s` and are not substitutions, because a
    /// letter is never a delimiter.
    #[test]
    fn the_other_s_commands_are_still_themselves() {
        let mut ed = editor("hello");

        ex(&mut ed, "set number 0");

        assert_eq!(ed.session.options.number, LineNumbers::Off);
    }

    /// `&` repeats the last substitute on the cursor's line — flags and all,
    /// which is the point of remembering the command rather than the pattern.
    #[test]
    fn ampersand_repeats_the_last_substitute_where_the_cursor_is() {
        let mut ed = editor("a a\na a\n");
        ex(&mut ed, "s/a/b/g");
        assert_eq!(rope_of(&ed), "b b\na a\n");

        ed.set_cursor(Cursor::at(4));
        ed.apply(cmd(Action::Ex { line: "&&".into(), run: true }));

        assert_eq!(rope_of(&ed), "b b\nb b\n", "the `g` came along");
    }

    #[test]
    fn g_ampersand_repeats_it_over_the_whole_file() {
        let mut ed = editor("a\na\na\n");
        ex(&mut ed, "s/a/b/");
        assert_eq!(rope_of(&ed), "b\na\na\n");

        ed.apply(cmd(Action::Ex { line: "%&&".into(), run: true }));

        assert_eq!(rope_of(&ed), "b\nb\nb\n");
    }

    #[test]
    fn ampersand_before_any_substitute_says_so() {
        let mut ed = editor("a\n");

        ed.apply(cmd(Action::Ex { line: "&&".into(), run: true }));

        assert_eq!(rope_of(&ed), "a\n");
        assert_eq!(ed.session.status, "no substitute to repeat");
    }

    /// The memory holds the pattern that *ran*: an empty pattern resolved to
    /// the search of that moment, and later searches do not rewrite history.
    #[test]
    fn the_repeat_keeps_the_pattern_that_was_in_force() {
        let mut ed = editor("one two\nnew two\n");
        ed.session.last_search =
            Some(Search { pattern: "two".into(), whole_word: false, forward: true });
        ex(&mut ed, "s//2/");
        assert_eq!(rope_of(&ed), "one 2\nnew two\n");

        ed.session.last_search =
            Some(Search { pattern: "new".into(), whole_word: false, forward: true });
        ed.set_cursor(Cursor::at(6));
        ed.apply(cmd(Action::Ex { line: "&&".into(), run: true }));

        assert_eq!(rope_of(&ed), "one 2\nnew 2\n", "still `two`, not `new`");
    }

    #[test]
    fn the_repeat_takes_a_range_and_refuses_an_argument() {
        let mut ed = editor("a\na\na\na\n");
        ex(&mut ed, "s/a/b/");

        ex(&mut ed, "2,3&&");
        assert_eq!(rope_of(&ed), "b\nb\nb\na\n");

        ex(&mut ed, "&& g");
        assert!(ed.session.status.starts_with("nothing goes after"), "{}", ed.session.status);
        assert_eq!(rope_of(&ed), "b\nb\nb\na\n");
    }

    // ---- :g, :v, :normal, :d ------------------------------------------------

    /// The first and the last line both go: the walk's row arithmetic holds
    /// at the ends, where off-by-ones live.
    #[test]
    fn global_delete_takes_exactly_the_matching_lines() {
        let mut ed = editor("foo one\nkeep\nfoo two\nkeep\nfoo three\n");

        ex(&mut ed, "g/foo/d");

        assert_eq!(rope_of(&ed), "keep\nkeep\n");
        assert_eq!(ed.session.status, "3 matching lines");

        ed.apply(cmd(Action::Undo));
        assert_eq!(rope_of(&ed), "foo one\nkeep\nfoo two\nkeep\nfoo three\n", "one `u`, not three");
    }

    #[test]
    fn vglobal_keeps_exactly_the_matching_lines() {
        let mut ed = editor("save a\ndrop\nsave b\n");

        ex(&mut ed, "v/save/d");

        assert_eq!(rope_of(&ed), "save a\nsave b\n");
    }

    #[test]
    fn g_bang_is_v() {
        let mut ed = editor("save a\ndrop\nsave b\n");

        ex(&mut ed, "g!/save/d");

        assert_eq!(rope_of(&ed), "save a\nsave b\n");
    }

    #[test]
    fn a_range_narrows_what_the_scan_reads() {
        let mut ed = editor("foo\nfoo\nfoo\nfoo\n");

        ex(&mut ed, "2,3g/foo/d");

        assert_eq!(rope_of(&ed), "foo\nfoo\n", "the first and last line were never scanned");
    }

    /// The `:g` pattern is the last search by the time the inner `s` asks —
    /// vim's idiom, and the reason `:g/foo/s//bar/g` reads as one thought.
    #[test]
    fn the_global_pattern_feeds_an_empty_substitute_pattern() {
        let mut ed = editor("foo x\nbar\nfoo y\n");

        ex(&mut ed, "g/foo/s//FOO/g");

        assert_eq!(rope_of(&ed), "FOO x\nbar\nFOO y\n");
    }

    /// The scan finishes before the first command runs: a command cannot
    /// edit a line into the match set.
    #[test]
    fn global_does_not_chase_its_own_output() {
        let mut ed = editor("a\nz\n");

        ex(&mut ed, "g/a/normal oa");

        assert_eq!(rope_of(&ed), "a\na\nz\n", "the line it made was not visited");
    }

    /// A whitelist, not a blacklist: the failure mode of a blacklist is
    /// `:g/x/q` closing the editor on the first match.
    #[test]
    fn global_refuses_a_command_that_is_not_line_scoped() {
        let mut ed = editor("a\n");

        ex(&mut ed, "g/a/q");

        assert_eq!(rope_of(&ed), "a\n");
        assert!(ed.session.status.contains("not `q`"), "{}", ed.session.status);
        assert!(!ed.session.quit, "and it did not quit");
    }

    #[test]
    fn global_with_no_match_says_so_and_runs_nothing() {
        let mut ed = editor("alpha\n");

        ex(&mut ed, "g/zebra/d");

        assert_eq!(rope_of(&ed), "alpha\n");
        assert_eq!(ed.session.status, "pattern not found: zebra");
    }

    #[test]
    fn global_with_no_command_asks_for_one() {
        let mut ed = editor("a\n");

        ex(&mut ed, "g/a/");

        assert_eq!(rope_of(&ed), "a\n");
        assert_eq!(ed.session.status, "and do what? `:g/pattern/d`");
    }

    /// `:vs` and `:vnew` still split — a letter is never a delimiter, so the
    /// `v` of `:v/pat/cmd` cannot swallow them.
    #[test]
    fn the_other_v_commands_are_still_themselves() {
        let mut ed = editor("a\n");
        sized(&mut ed);

        ex(&mut ed, "vs");

        assert_eq!(ed.window_ids().len(), 2, "it split, it did not scan");
    }

    #[test]
    fn normal_replays_keys_and_returns_to_normal_mode() {
        let mut ed = editor("fix\n");

        ex(&mut ed, "normal A!");

        assert_eq!(rope_of(&ed), "fix!\n", "`A` entered insert and `!` was typed");
        assert_eq!(ed.session.mode, Mode::Normal, "the Esc was pressed for you");
    }

    #[test]
    fn normal_under_a_range_runs_once_per_row_as_one_undo_step() {
        let mut ed = editor("one\ntwo\nthree\n");

        ex(&mut ed, "%normal I//");

        assert_eq!(rope_of(&ed), "//one\n//two\n//three\n");

        ed.apply(cmd(Action::Undo));
        assert_eq!(rope_of(&ed), "one\ntwo\nthree\n", "one `u`, not three");
    }

    #[test]
    fn normal_with_nothing_to_type_asks() {
        let mut ed = editor("a\n");

        ex(&mut ed, "normal");

        assert!(ed.session.status.starts_with("normal what?"), "{}", ed.session.status);
    }

    /// `:g/TODO/normal A // fixme` — the line this feature was asked with.
    #[test]
    fn global_normal_appends_to_every_match() {
        let mut ed = editor("code();\ntodo one\ncode();\ntodo two\n");

        ex(&mut ed, "g/todo/normal A // fixme");

        assert_eq!(rope_of(&ed), "code();\ntodo one // fixme\ncode();\ntodo two // fixme\n");
    }

    #[test]
    fn d_deletes_the_cursors_line_and_a_range_deletes_its_rows() {
        let mut ed = editor("a\nb\nc\nd\n");
        ed.set_cursor(Cursor::at(2));
        ex(&mut ed, "d");
        assert_eq!(rope_of(&ed), "a\nc\nd\n");
        assert_eq!(ed.session.status, "1 fewer line");

        let mut ed = editor("a\nb\nc\nd\n");
        ex(&mut ed, "2,3d");
        assert_eq!(rope_of(&ed), "a\nd\n");
        assert_eq!(ed.session.status, "2 fewer lines");

        ed.apply(cmd(Action::Undo));
        assert_eq!(rope_of(&ed), "a\nb\nc\nd\n", "one undo step");
    }

    /// `:delete` is a path and `:d` is lines; the argument rule keeps a typo
    /// in one from reaching the other.
    #[test]
    fn d_refuses_an_argument() {
        let mut ed = editor("a\nb\n");

        ex(&mut ed, "d 4");

        assert_eq!(rope_of(&ed), "a\nb\n");
        assert!(ed.session.status.starts_with("`:d` deletes lines"), "{}", ed.session.status);
    }

    // ---- boundaries: ]], [[, :ts, :peek -------------------------------------

    /// A parsed buffer with a function, its parameters, and a body — enough
    /// structure for every boundary case.
    fn rust_editor() -> Editor {
        let mut ed = editor("fn add(a: i32, b: i32) {\n    a + b;\n}\n");
        ex(&mut ed, "set syntax rust");
        ed.set_cursor(Cursor::at(0));
        ed
    }

    /// From the top: the `(`, each parameter's ends, the `)`, the `{`, and
    /// the shared final `}` — starts and ends both stops.
    #[test]
    fn boundary_jump_walks_blocks_and_arguments() {
        let mut ed = rust_editor();

        for want in [6, 7, 12, 15, 20, 21, 23, 36] {
            ed.apply(cmd(Action::BoundaryJump { forward: true }));
            assert_eq!(ed.cursor().unwrap().at, want);
        }
        ed.apply(cmd(Action::BoundaryJump { forward: true }));
        assert_eq!(ed.cursor().unwrap().at, 36, "the last boundary does not wrap");

        ed.apply(cmd(Action::BoundaryJump { forward: false }));
        assert_eq!(ed.cursor().unwrap().at, 23, "`[[` walks the same stops back");
    }

    #[test]
    fn a_boundary_jump_without_a_tree_says_so() {
        let mut ed = editor("plain text\n");

        ed.apply(cmd(Action::BoundaryJump { forward: true }));

        assert_eq!(ed.session.status, "no syntax tree here");
        assert_eq!(ed.cursor().unwrap().at, 0, "and the cursor stayed");
    }

    #[test]
    fn ts_toggles_both_ways_and_reports() {
        let mut ed = rust_editor();

        ex(&mut ed, "ts");
        assert!(ed.session.ts_marks);
        assert_eq!(ed.session.status, "boundaries on");

        ex(&mut ed, "ts");
        assert!(!ed.session.ts_marks);
        assert_eq!(ed.session.status, "boundaries off");
    }

    #[test]
    fn ts_without_a_tree_stays_off_and_says_why() {
        let mut ed = editor("plain\n");

        ex(&mut ed, "ts");

        assert!(!ed.session.ts_marks);
        assert_eq!(ed.session.status, "no syntax tree here");
    }

    /// The `s` treatment: the dim under everything, a mark per boundary —
    /// and only where you are looking.
    #[test]
    fn ts_decorations_dim_and_mark_the_focused_window_only() {
        let mut ed = rust_editor();
        sized(&mut ed);
        ex(&mut ed, "vs");
        ex(&mut ed, "ts");

        let dim = ed.theme.ui.dim;
        let mark = ed.theme.ui.search;
        let repaints = |ed: &Editor, id| {
            ed.decorations(id, 0..3)
                .into_iter()
                .filter_map(|d| match d {
                    Decoration::Repaint { range, style, .. } => Some((range, style)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        let focused = repaints(&ed, ed.focus());
        assert!(focused.iter().any(|(_, style)| *style == dim), "the dim went down");
        assert!(
            focused.iter().any(|(range, style)| *style == mark && range.start == 6),
            "the `(` wears a mark: {focused:?}"
        );

        let other = ed.window_ids().into_iter().find(|&id| id != ed.focus()).unwrap();
        assert!(
            !repaints(&ed, other).iter().any(|(_, style)| *style == dim),
            "the unfocused window reads normally"
        );
    }

    // ---- splitjoin: :tssplit, :tsjoin ---------------------------------------

    #[test]
    fn tssplit_breaks_the_list_and_reindents_like_equals() {
        let mut ed = editor("fn add(a: i32, b: i32) {}\n");
        ex(&mut ed, "set syntax rust");
        ed.set_cursor(Cursor::at(8));

        ex(&mut ed, "tssplit");

        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "fn add(\n    a: i32,\n    b: i32\n) {}\n"
        );
    }

    #[test]
    fn tssplit_finishes_a_half_split_list_without_blank_lines() {
        let mut ed = editor("fn add(\n    a: i32, b: i32) {}\n");
        ex(&mut ed, "set syntax rust");
        ed.set_cursor(Cursor::at(12));

        ex(&mut ed, "tssplit");

        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "fn add(\n    a: i32,\n    b: i32\n) {}\n"
        );
    }

    #[test]
    fn tsjoin_flattens_the_list_and_drops_the_trailing_comma() {
        let mut ed = editor("fn add(\n    a: i32,\n    b: i32,\n) {}\n");
        ex(&mut ed, "set syntax rust");
        ed.set_cursor(Cursor::at(12));

        ex(&mut ed, "tsjoin");

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "fn add(a: i32, b: i32) {}\n");
    }

    #[test]
    fn tssplit_then_tsjoin_is_one_undo_step_each() {
        let mut ed = editor("call(a, b)\n");
        ex(&mut ed, "set syntax rust");
        ed.set_cursor(Cursor::at(6));

        ex(&mut ed, "tssplit");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "call(\n    a,\n    b\n)\n");
        // The settle every keystroke gets: the tree follows the edit before
        // the next command reads it.
        ed.settle();
        ex(&mut ed, "tsjoin");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "call(a, b)\n");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "call(\n    a,\n    b\n)\n");
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "call(a, b)\n");
    }

    #[test]
    fn splitjoin_says_why_it_did_nothing() {
        let mut ed = editor("plain text\n");
        ex(&mut ed, "tssplit");
        assert_eq!(ed.session.status, "no syntax tree here");

        let mut ed = rust_editor();
        ex(&mut ed, "tsjoin");
        assert_eq!(ed.session.status, "no brackets around the cursor");

        let mut ed = editor("call(a, b)\n");
        ex(&mut ed, "set syntax rust");
        ed.set_cursor(Cursor::at(6));
        ex(&mut ed, "tsjoin");
        assert_eq!(ed.session.status, "already one line");
    }

    // ---- zen ----------------------------------------------------------------

    #[test]
    fn zen_toggles_both_ways_and_reports() {
        let mut ed = editor("x\n");
        assert!(!ed.session.zen);

        ex(&mut ed, "zen");
        assert!(ed.session.zen);
        assert_eq!(ed.session.status, "zen on");

        ex(&mut ed, "zen");
        assert!(!ed.session.zen);
        assert_eq!(ed.session.status, "zen off");
    }

    // ---- git signs: the gutter, the numstat ---------------------------------

    #[test]
    fn git_signs_follow_edits_through_the_drain() {
        let f = Scratch::new("git-signs", "a\nb\nc\n");
        let mut ed = opened(&f);
        ed.set_git_baseline(|_| Some("a\nb\nc\n".into()));

        assert!(ed.gutter_signs(ed.focus(), 0..3).is_empty(), "clean file, no signs");
        assert!(ed.git_stats(ed.focus()).unwrap().is_clean());

        // Rewrite row 1: the sign lands with the settle, like the parse tree.
        ed.buffer_mut().unwrap().replace_range(2, 3, "B");
        ed.settle();

        let signs = ed.gutter_signs(ed.focus(), 0..3);
        assert_eq!(signs.len(), 1);
        assert_eq!((signs[0].0, signs[0].1), (1, '▎'));
        assert_eq!(signs[0].2, ed.theme.ui.git_change);
        let stats = ed.git_stats(ed.focus()).unwrap();
        assert_eq!((stats.added, stats.changed, stats.removed), (0, 1, 0));
    }

    #[test]
    fn no_baseline_means_no_signs_and_no_numstat() {
        let f = Scratch::new("git-untracked", "a\n");
        let mut ed = opened(&f);
        ed.set_git_baseline(|_| None);

        ed.buffer_mut().unwrap().insert_str(Cursor::at(0), "x");
        ed.settle();

        assert!(ed.gutter_signs(ed.focus(), 0..1).is_empty());
        assert!(ed.git_stats(ed.focus()).is_none());
    }

    #[test]
    fn set_git_signs_false_hides_the_lot_without_forgetting_it() {
        let f = Scratch::new("git-off", "a\n");
        let mut ed = opened(&f);
        ed.set_git_baseline(|_| Some("different\n".into()));
        assert!(!ed.gutter_signs(ed.focus(), 0..1).is_empty());

        ex(&mut ed, "set git_signs false");
        assert!(ed.gutter_signs(ed.focus(), 0..1).is_empty());
        assert!(ed.git_stats(ed.focus()).is_none());

        ex(&mut ed, "set git_signs true");
        assert!(!ed.gutter_signs(ed.focus(), 0..1).is_empty(), "and back, nothing recomputed");
    }

    #[test]
    fn a_revert_rereads_the_baseline() {
        let f = Scratch::new("git-revert", "a\n");
        let mut ed = opened(&f);
        // A loader whose answer changes: the index moved between calls.
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = calls.clone();
        ed.set_git_baseline(move |_| {
            counted.set(counted.get() + 1);
            Some("a\n".into())
        });
        assert_eq!(calls.get(), 1, "installing fetches");

        ex(&mut ed, "e");
        assert_eq!(calls.get(), 2, "reverting fetches again");
    }

    #[test]
    fn diags_with_nothing_stored_says_so() {
        let mut ed = editor("x\n");
        ex(&mut ed, "diags");
        assert_eq!(ed.session.status, "no diagnostics");
    }

    #[test]
    fn peek_without_a_server_says_why_and_does_not_split() {
        let mut ed = editor("fn main() {}\n");
        sized(&mut ed);
        let before = ed.window_ids().len();

        ex(&mut ed, "peek");

        assert_eq!(ed.window_ids().len(), before, "no empty split left behind");
        assert!(ed.session.status.starts_with("lsp:"), "{}", ed.session.status);
    }

    // ---- gq, reflow ---------------------------------------------------------

    #[test]
    fn gqq_wraps_the_cursors_line_to_textwidth() {
        let mut ed = editor("one two three four five\n");
        ex(&mut ed, "set textwidth 10");

        ed.apply(operate(Operator::Reflow, Motion::CurrentLine, 1));

        assert_eq!(rope_of(&ed), "one two\nthree four\nfive\n");
    }

    #[test]
    fn gqip_wraps_the_paragraph_and_leaves_its_neighbour() {
        let mut ed = editor("aaa bbb\nccc\n\nnext one\n");
        ex(&mut ed, "set textwidth 40");

        ed.apply(cmd(Action::Operate {
            op: Operator::Reflow,
            target: Target::Object { object: TextObject::Paragraph, around: false },
            count: 1,
            sink: Sink::Ring,
        }));

        assert_eq!(rope_of(&ed), "aaa bbb ccc\n\nnext one\n");
    }

    /// Vim's habit: the cursor at the end of what was formatted, ready to
    /// continue below it.
    #[test]
    fn the_reflow_cursor_lands_on_the_last_line_produced() {
        let mut ed = editor("one two three\nrest\n");
        ex(&mut ed, "set textwidth 8");

        ed.apply(operate(Operator::Reflow, Motion::CurrentLine, 1));

        assert_eq!(rope_of(&ed), "one two\nthree\nrest\n");
        let row = ed.buffer().unwrap().row_at(ed.cursor().unwrap());
        assert_eq!(row, 1);
    }

    #[test]
    fn a_comment_reflow_keeps_its_leader_and_is_one_undo_step() {
        let mut ed = editor("// alpha beta gamma\n");
        ex(&mut ed, "set textwidth 11");

        ed.apply(operate(Operator::Reflow, Motion::CurrentLine, 1));
        assert_eq!(rope_of(&ed), "// alpha\n// beta\n// gamma\n");

        ed.apply(cmd(Action::Undo));
        assert_eq!(rope_of(&ed), "// alpha beta gamma\n");
    }

    #[test]
    fn visual_gq_wraps_the_selected_rows_and_collapses() {
        let mut ed = editor("one two three four\nrest\n");
        ex(&mut ed, "set textwidth 10");
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
        ed.apply(cmd(Action::Move(Motion::Right)));

        ed.apply(cmd(Action::OperateSelection { op: Operator::Reflow, sink: Sink::Ring }));

        assert_eq!(rope_of(&ed), "one two\nthree four\nrest\n");
        assert_eq!(ed.session.mode, Mode::Normal, "the selection was consumed");
    }

    // ---- =, reindent --------------------------------------------------------

    #[test]
    fn double_equals_reindents_the_cursors_line_by_bracket_depth() {
        let mut ed = editor("fn f() {\nx();\n}\n");
        ed.set_cursor(Cursor::at(9));

        ed.apply(operate(Operator::Reindent, Motion::CurrentLine, 1));

        assert_eq!(rope_of(&ed), "fn f() {\n    x();\n}\n");
    }

    /// `gg=G` is this: the cursor at the top, `=` to the last line. One undo
    /// step, and the closers sit with the lines that opened them.
    #[test]
    fn equals_to_the_last_line_reindents_the_file() {
        let mut ed = editor("fn f() {\n        a();\nif b {\nc();\n}\n}\n");

        ed.apply(operate(Operator::Reindent, Motion::LastLine, 1));

        assert_eq!(rope_of(&ed), "fn f() {\n    a();\n    if b {\n        c();\n    }\n}\n");

        ed.apply(cmd(Action::Undo));
        assert_eq!(rope_of(&ed), "fn f() {\n        a();\nif b {\nc();\n}\n}\n", "one `u`");
    }

    /// The lines above are context and the lines below are not touched: `==`
    /// still knows its depth, and fixes only what it was aimed at.
    #[test]
    fn a_line_outside_the_reindented_range_is_untouched() {
        let mut ed = editor("{\nx\ny\n}\n");
        ed.set_cursor(Cursor::at(2));

        ed.apply(operate(Operator::Reindent, Motion::CurrentLine, 1));

        assert_eq!(rope_of(&ed), "{\n    x\ny\n}\n", "`y` keeps its wrong indent");
    }

    #[test]
    fn visual_equals_reindents_the_selected_rows_and_collapses() {
        let mut ed = editor("{\nx\n}\n");
        ed.apply(cmd(Action::EnterVisual(Shape::Lines)));
        ed.apply(cmd(Action::Move(Motion::Down)));

        ed.apply(cmd(Action::OperateSelection { op: Operator::Reindent, sink: Sink::Ring }));

        assert_eq!(rope_of(&ed), "{\n    x\n}\n");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn dot_repeats_a_reflow() {
        let mut ed = editor("aaa bbb ccc\n\nddd eee fff\n");
        ex(&mut ed, "set textwidth 7");

        ed.apply(operate(Operator::Reflow, Motion::CurrentLine, 1));
        assert_eq!(rope_of(&ed), "aaa bbb\nccc\n\nddd eee fff\n");

        let at = rope_of(&ed).find("ddd").unwrap();
        ed.set_cursor(Cursor::at(at));
        ed.apply(cmd(Action::RepeatChange { count: None }));

        assert_eq!(rope_of(&ed), "aaa bbb\nccc\n\nddd eee\nfff\n");
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

    /// `syntax_for` reads the whole file name rather than the extension, so a
    /// grammar can claim a file that has none worth having. `CMakeLists.txt`
    /// looks like a `.txt` from the outside.
    #[test]
    fn a_grammar_can_claim_a_file_by_name() {
        let d = ScratchDir::new("byname").file("CMakeLists.txt").file("notes.txt");

        let cmake = Editor::open(format!("{}/CMakeLists.txt", d.path())).unwrap();
        assert!(cmake.syntax().is_some(), "CMakeLists.txt is cmake, not plain text");

        let plain = Editor::open(format!("{}/notes.txt", d.path())).unwrap();
        assert!(plain.syntax().is_none(), "an ordinary .txt still has no grammar");
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

    // ---- moving lines -------------------------------------------------------

    fn whole(ed: &Editor) -> String {
        ed.buffer().unwrap().rope().to_string()
    }

    fn move_key(ed: &mut Editor, down: bool, count: usize) {
        ed.apply(Command { count, action: Action::MoveLines { down } });
    }

    #[test]
    fn shift_down_moves_the_line_and_the_cursor_rides_with_it() {
        let mut ed = editor("alpha\nbeta\ngamma\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(0, false));

        move_key(&mut ed, true, 1);

        assert_eq!(whole(&ed), "beta\nalpha\ngamma\n");
        assert_eq!(ed.cursor_row().unwrap(), 1, "still on the line it pushed");
    }

    #[test]
    fn a_count_carries_the_line_that_many_rows() {
        let mut ed = editor("a\nb\nc\nd\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(0, false));

        move_key(&mut ed, true, 2);

        assert_eq!(whole(&ed), "b\nc\na\nd\n");
    }

    #[test]
    fn moving_off_either_end_does_nothing_at_all() {
        let mut ed = editor("a\nb\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(0, false));
        let edits = ed.buffer().unwrap().edits();

        move_key(&mut ed, false, 1);

        assert_eq!(whole(&ed), "a\nb\n");
        assert_eq!(ed.buffer().unwrap().edits(), edits, "and leaves no undo step");
    }

    /// The signed forms are addresses too — `+3` is `.+3` — so `:m -1` names
    /// the line above and moves nothing. Every line here is what vim 9.0
    /// prints for the same keystrokes.
    #[test]
    fn the_signed_forms_are_addresses_relative_to_the_cursor() {
        let at = |arg: &str| {
            let mut ed = editor("a\nb\nc\nd\ne\n");
            ed.set_cursor(ed.buffer().unwrap().at_row(2, false));
            ex(&mut ed, arg);
            whole(&ed)
        };

        assert_eq!(at("m +1"), "a\nb\nd\nc\ne\n");
        assert_eq!(at("m +2"), "a\nb\nd\ne\nc\n");
        assert_eq!(at("m -2"), "a\nc\nb\nd\ne\n");
        assert_eq!(at("m -1"), "a\nb\nc\nd\ne\n", "names the line above, so nothing");
        assert_eq!(at("m +"), "a\nb\nd\nc\ne\n", "a bare sign is one");
        assert_eq!(at("m -"), "a\nb\nc\nd\ne\n");
    }

    #[test]
    fn the_ex_command_reaches_the_top_and_the_bottom() {
        let mut ed = editor("a\nb\nc\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(1, false));
        ex(&mut ed, "m 0");
        assert_eq!(whole(&ed), "b\na\nc\n");

        ex(&mut ed, "m $");
        assert_eq!(whole(&ed), "a\nc\nb\n");
    }

    /// A bare number is vim's address — the lines land *after* line N — and
    /// these are what vim 9.0 actually prints, measured rather than remembered.
    /// Note it is direction-dependent by nature: from above, "after line 4"
    /// makes it line 4; from below, "after line 2" makes it line 3.
    #[test]
    fn a_bare_number_is_an_address_and_the_lines_land_after_it() {
        let at = |row: usize, arg: &str| {
            let mut ed = editor("a\nb\nc\nd\ne\n");
            ed.set_cursor(ed.buffer().unwrap().at_row(row, false));
            ex(&mut ed, arg);
            whole(&ed)
        };

        assert_eq!(at(1, "m 4"), "a\nc\nd\nb\ne\n", "from above: b becomes line 4");
        assert_eq!(at(4, "m 2"), "a\nb\ne\nc\nd\n", "from below: e becomes line 3");
        assert_eq!(at(3, "m 4"), "a\nb\nc\nd\ne\n", "already after line 4, so nothing");
        assert_eq!(at(1, "m 0"), "b\na\nc\nd\ne\n", "0 is before line 1");
        assert_eq!(at(1, "m $"), "a\nc\nd\ne\nb\n");
    }

    /// `:m .+1` and `:m+1` are the same command, and both are how a decade of
    /// vimrcs write it. Neither used to reach `:m` at all: one was an address
    /// that did not parse, the other a command called `m+1`.
    #[test]
    fn the_address_may_spell_the_cursors_line_and_may_touch_the_command() {
        let at = |row: usize, arg: &str| {
            let mut ed = editor("a\nb\nc\nd\ne\n");
            ed.set_cursor(ed.buffer().unwrap().at_row(row, false));
            ex(&mut ed, arg);
            whole(&ed)
        };

        // The two every vimrc binds, written out and glued on.
        for down in ["m .+1", "m+1", "move.+1"] {
            assert_eq!(at(1, down), "a\nc\nb\nd\ne\n", "{down}");
        }
        for up in ["m .-2", "m-2", "move-2"] {
            assert_eq!(at(2, up), "a\nc\nb\nd\ne\n", "{up}");
        }

        assert_eq!(at(1, "m."), "a\nb\nc\nd\ne\n", "the cursor's own line moves nothing");
        assert_eq!(at(1, "m .-1"), "a\nb\nc\nd\ne\n", "and neither does the line above it");
        assert_eq!(at(1, "m$"), "a\nc\nd\ne\nb\n");
        assert_eq!(at(1, "m0"), "b\na\nc\nd\ne\n");
    }

    /// A range in front of the command says which lines, whatever the
    /// selection is — which is the whole reason `:2,5m` exists rather than
    /// "select it first". See `docs/specs/ranges.md`.
    #[test]
    fn a_range_says_which_lines_move_and_no_range_means_the_selection() {
        let mut ed = editor("a\nb\nc\nd\ne\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(0, false));
        ex(&mut ed, "2,3m $");
        assert_eq!(whole(&ed), "a\nd\ne\nb\nc\n", "the range, not the cursor's line");

        // The same command with no range moves what is selected.
        let mut ed = editor("a\nb\nc\nd\ne\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(1, false));
        ed.apply(cmd(Action::EnterVisual(Shape::Lines)));
        ed.apply(cmd(Action::Move(Motion::Down)));
        ex(&mut ed, "m $");
        assert_eq!(whole(&ed), "a\nd\ne\nb\nc\n");

        // And `%` is every line, which moves them all and changes nothing.
        let mut ed = editor("a\nb\nc\n");
        ex(&mut ed, "%m 0");
        assert_eq!(whole(&ed), "a\nb\nc\n");
    }

    /// A range's lines have to exist, unlike `:m`'s argument, which may name
    /// line 0 because it is a line to land *after*.
    #[test]
    fn a_range_past_the_end_is_refused_and_names_the_line() {
        let mut ed = editor("a\nb\nc\n");
        ex(&mut ed, "2,9m 0");
        assert_eq!(ed.session.status, "no line 9");
        assert_eq!(whole(&ed), "a\nb\nc\n", "and nothing moved");
    }

    /// A range handed to a command that has no use for one is an error, not a
    /// range quietly dropped: vim writes part of a file for `:1,5w`, and a
    /// command that ignores half of what you typed is the worse of the two
    /// ways to not support something.
    #[test]
    fn a_command_that_takes_no_range_says_so_rather_than_ignoring_it() {
        let mut ed = editor("a\nb\nc\n");
        // Writing part of a file is the example: vim does it, and a `:w` that
        // silently wrote all of it would be the worse of the two ways to not
        // support a range.
        for line in ["1,5w out.txt", "2q", "'<,'>e other.txt", "1,2reload"] {
            ex(&mut ed, line);
            assert!(ed.session.status.ends_with("takes no range"), "{line}: {}", ed.session.status);
        }
        assert_eq!(whole(&ed), "a\nb\nc\n");
    }

    /// A range with no command goes to its last line. `:42` was a special case
    /// in the parser; now it is this rule falling out, and `:$` and `:%` come
    /// with it.
    #[test]
    fn a_range_with_no_command_goes_to_its_last_line() {
        let row = |line: &str| {
            let mut ed = editor("a\nb\nc\nd\ne\n");
            ed.set_cursor(ed.buffer().unwrap().at_row(0, false));
            ex(&mut ed, line);
            ed.cursor_row().unwrap()
        };

        assert_eq!(row("3"), 2, "one-based on the way in");
        assert_eq!(row("$"), 4);
        assert_eq!(row("%"), 4, "the last line of `1,$`");
        assert_eq!(row("+2"), 2, "an offset from where the cursor is");
        assert_eq!(row("2,4"), 3, "the last line of the range");
        // Clamped rather than refused, unlike a range's own lines: this is the
        // oldest spelling of "take me there".
        assert_eq!(row("99"), 4);
        assert_eq!(row("0"), 0);
    }

    /// The split only fires where an address is stuck to the name, or every
    /// command starting with `m` would lose its first letter.
    #[test]
    fn a_command_that_merely_begins_with_m_is_not_a_move() {
        let mut ed = editor("a\nb\n");
        ex(&mut ed, "mark");
        assert_eq!(ed.session.status, "not a command: mark");
        ex(&mut ed, "move");
        assert!(ed.session.status.starts_with("move where?"), "{}", ed.session.status);
        assert_eq!(whole(&ed), "a\nb\n", "and the buffer is untouched by either");
    }

    /// The address arithmetic has to count the block's own rows: from above,
    /// the address falls through the hole the block leaves. Both of these are
    /// what `:2,3m {addr}` prints in vim 9.0.
    #[test]
    fn a_block_lands_after_the_address_from_either_side_of_it() {
        let at = |arg: &str| {
            let mut ed = editor("a\nb\nc\nd\ne\n");
            ed.set_cursor(ed.buffer().unwrap().at_row(1, false));
            ed.apply(cmd(Action::EnterVisual(Shape::Lines)));
            ed.apply(cmd(Action::Move(Motion::Down)));
            ex(&mut ed, arg);
            whole(&ed)
        };

        assert_eq!(at("m 5"), "a\nd\ne\nb\nc\n", "from above line 5");
        assert_eq!(at("m 0"), "b\nc\na\nd\ne\n", "to the top");
        assert_eq!(at("m 1"), "a\nb\nc\nd\ne\n", "already after line 1");
    }

    /// A typed address is a claim about a line that either exists or does not,
    /// so vim refuses it rather than doing its best. The arrow keys, which
    /// name no line, still clamp.
    #[test]
    fn an_address_off_either_end_is_refused_rather_than_clamped() {
        let mut ed = editor("a\nb\nc\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(0, false));

        ex(&mut ed, "m +99");
        assert_eq!(whole(&ed), "a\nb\nc\n", "{}", ed.session.status);
        assert_eq!(ed.session.status, "no line 100");

        ex(&mut ed, "m 9");
        assert_eq!(whole(&ed), "a\nb\nc\n");

        move_key(&mut ed, true, 99);
        assert_eq!(whole(&ed), "b\nc\na\n", "but Shift-Down still just stops");
    }

    /// Keeping the selection is what makes nudging a block a matter of holding
    /// a key rather than counting rows first.
    #[test]
    fn a_visual_block_moves_as_one_and_stays_selected() {
        let mut ed = editor("a\nb\nc\nd\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(0, false));
        ed.apply(cmd(Action::EnterVisual(Shape::Lines)));
        ed.apply(cmd(Action::Move(Motion::Down)));

        move_key(&mut ed, true, 1);
        assert_eq!(whole(&ed), "c\na\nb\nd\n");

        move_key(&mut ed, true, 1);
        assert_eq!(whole(&ed), "c\nd\na\nb\n", "the second press carried on");
    }

    #[test]
    fn undo_puts_a_move_back_in_one_step() {
        let mut ed = editor("a\nb\nc\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(0, false));

        move_key(&mut ed, true, 1);
        assert_eq!(whole(&ed), "b\na\nc\n");
        ed.apply(cmd(Action::Undo));

        assert_eq!(whole(&ed), "a\nb\nc\n");
    }

    /// The property `settle` already gives every other edit, and the reason a
    /// move goes through `edit_raw` like anything else.
    #[test]
    fn another_window_follows_the_lines_that_moved() {
        let mut ed = editor("a\nb\nc\nd\n");
        ed.set_cursor(ed.buffer().unwrap().at_row(3, false));
        split(&mut ed, Dir::Horizontal);
        let watcher = other(&ed);
        ed.set_cursor(ed.buffer().unwrap().at_row(0, false));

        move_key(&mut ed, true, 1);
        ed.settle();

        let cursor = row_in(&ed, watcher);
        assert_eq!(cursor, 3, "still on `d`, which the move did not touch");
    }

    fn row_in(ed: &Editor, id: WindowId) -> usize {
        let view = ed.window_of(id).unwrap().text().unwrap();
        ed.buffer_of(id).unwrap().row_at(view.selections.cursor())
    }

    // ---- the clipboard ------------------------------------------------------

    fn marked(ed: &Editor) -> Vec<String> {
        ed.session
            .clipboard
            .marks()
            .iter()
            .map(|m| m.path.file_name().unwrap().to_string_lossy().into_owned())
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

    /// The verb belongs to the path, so one paste can do both. The mark column
    /// is what makes that safe to offer — every row shows which it is.
    #[test]
    fn one_paste_copies_some_and_moves_others() {
        let d = ScratchDir::new("paste-mixed").file("a.rs").file("b.rs").dir("pkg");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed); // pkg/ sorts first
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Copy)); // a.rs
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });
        tree_key(&mut ed, TreeCmd::Mark(ClipMode::Cut)); // b.rs

        assert_eq!(ed.session.status, "1 to copy, 1 to move", "the footer says both halves");

        tree_key(&mut ed, TreeCmd::Select { down: false, count: 2 });
        tree_key(&mut ed, TreeCmd::Paste);

        let at = |p: &str| std::path::Path::new(&format!("{}/{p}", d.path())).exists();
        assert!(at("pkg/a.rs") && at("pkg/b.rs"), "both landed");
        assert!(at("a.rs"), "the copy is still where it was");
        assert!(!at("b.rs"), "the cut is not");
        assert_eq!(marked(&ed), ["a.rs"], "the spent cut is forgotten, the copy is not");
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

    /// The `bi .` session: the tree is the only window, so there is nothing
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

        // The tree is the leftmost pane, so the layout is tree, first, second
        // and a single step reaches it from either one — forwards off the end
        // from `second`, and backwards off the start from `first`. That is
        // what makes "the window you came from" the thing under test rather
        // than the cycling order.
        while ed.focus() != first {
            ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        }
        ex(&mut ed, &format!("vs {}", d.path()));
        let tree = ed.focus();

        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Enter);
        assert_eq!(ed.focus(), first, "came from the first, went back to it");

        // Now reach the tree from the *second* window instead.
        while ed.focus() != second {
            ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        }
        ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        assert_eq!(ed.focus(), tree, "one step onto the tree");
        tree_key(&mut ed, TreeCmd::Enter);

        assert_eq!(ed.focus(), second, "and this time back to the second");
    }

    fn tree_key(ed: &mut Editor, tree_cmd: TreeCmd) {
        ed.apply(cmd(Action::Tree(tree_cmd)));
    }

    /// Down one row from the root, onto the first entry.
    fn select_first_entry(ed: &mut Editor) {
        tree_key(ed, TreeCmd::Select { down: true, count: 1 });
    }

    /// Every item the tree's fuzzy list is offering, in the order it offers
    /// them once the query has been applied.
    fn offered(ed: &Editor) -> Vec<String> {
        let picker = ed.session.picker.as_ref().expect("a list is up");
        picker.matches().iter().map(|&i| picker.items()[i].text.clone()).collect()
    }

    fn find_in_tree(ed: &mut Editor, whole: bool, query: &str) {
        tree_key(ed, TreeCmd::Find { whole });
        for c in query.chars() {
            ed.apply(cmd(Action::PickChar(c)));
        }
    }

    /// `gf` in a tree is the same question as `gf` in a text window — go to a
    /// thing by name — over the things this pane has. Taking one moves the
    /// selection to it and opens nothing.
    #[test]
    fn gf_in_a_tree_finds_a_row_and_selects_it() {
        let d = ScratchDir::new("tree-pick").file("alpha.rs").file("beta.rs").file("gamma.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        assert_eq!(ed.window().tree().unwrap().selected(), 0, "on the root");

        find_in_tree(&mut ed, true, "");
        assert_eq!(ed.session.mode, Mode::Pick);
        assert_eq!(offered(&ed), ["alpha.rs", "beta.rs", "gamma.rs"], "the rows, not the root");

        ed.apply(cmd(Action::PickChar('g')));
        ed.apply(cmd(Action::PickAccept));

        let tree = ed.window().tree().expect("still a tree, because nothing was opened");
        assert_eq!(tree.selected_row().unwrap().name, "gamma.rs");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    /// `gf` searches the whole tree, because a list of only the rows that
    /// happen to be expanded hides the file you are looking for behind the
    /// directories you have not opened — which is a tree's usual state. Taking
    /// one opens the way down to it.
    #[test]
    fn gf_reaches_a_file_inside_a_directory_that_is_still_closed() {
        let d = ScratchDir::new("tree-pick-deep").dir("pkg").file("pkg/deep.rs").file("top.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        assert_eq!(offered_with(&mut ed, false), ["pkg", "top.rs"], "not on the pane yet");

        find_in_tree(&mut ed, true, "deep");
        assert_eq!(offered(&ed), ["pkg/deep.rs"]);
        ed.apply(cmd(Action::PickAccept));

        let tree = ed.window().tree().expect("still a tree");
        assert_eq!(tree.selected_row().unwrap().name, "deep.rs");
        assert!(tree.rows().iter().any(|r| r.name == "pkg" && r.open), "the way down was opened");
    }

    /// `/` is the same list narrowed to the pane you can see — the trade for
    /// when the whole disk is not what you meant.
    #[test]
    fn slash_searches_only_what_is_on_screen() {
        let d = ScratchDir::new("tree-pick-visible").dir("pkg").file("pkg/deep.rs").file("top.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        assert_eq!(
            offered_with(&mut ed, false),
            ["pkg", "top.rs"],
            "the closed one hides its file"
        );

        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Expand);
        assert_eq!(offered_with(&mut ed, false), ["pkg", "pkg/deep.rs", "top.rs"], "once open");
    }

    /// A row already on screen wins a tie and loses to a genuinely better
    /// match. The stable sort is the whole mechanism: the visible rows go in
    /// first, and only a higher score moves anything past them.
    #[test]
    fn an_open_row_wins_a_tie_and_a_better_match_wins_outright() {
        let d = ScratchDir::new("tree-pick-rank")
            .dir("pkg")
            .file("pkg/thing.rs")
            .file("pkg/x_thing.rs")
            .file("thing.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        find_in_tree(&mut ed, true, "thing");
        let ranked = offered(&ed);
        assert_eq!(ranked[0], "thing.rs", "on screen, and no worse a match than the others");
        assert!(ranked.contains(&"pkg/thing.rs".to_string()), "the closed ones are still offered");

        // A query only the buried file matches well: being on screen does not
        // save `thing.rs` from a match that is genuinely better.
        find_in_tree(&mut ed, true, "xth");
        assert_eq!(offered(&ed)[0], "pkg/x_thing.rs");
    }

    /// Opens the list, reads it, and closes it again.
    fn offered_with(ed: &mut Editor, whole: bool) -> Vec<String> {
        tree_key(ed, TreeCmd::Find { whole });
        let out = offered(ed);
        ed.apply(cmd(Action::PickCancel));
        out
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

    /// `bi .` scopes the session to the directory it was given, and opening a
    /// file out of the tree must not move that. The root is a thing you set,
    /// with `+` and `-`; nothing else may set it for you.
    #[test]
    fn opening_a_file_out_of_the_tree_keeps_the_root_you_opened() {
        let d = ScratchDir::new("keep-root").file("pkg/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Expand);
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });

        tree_key(&mut ed, TreeCmd::Enter);
        // `-` out of the file it just opened, which is how you get the tree
        // back when it was the only window.
        tree_key(&mut ed, TreeCmd::Up);

        let tree = ed.window().tree().expect("the tree is back");
        assert_eq!(tree.root(), std::path::Path::new(d.path()), "the directory bi opened");
        assert_eq!(
            tree.selected_row().unwrap().name,
            "a.rs",
            "with the file you left revealed, however deep it sits",
        );
    }

    /// The other half of the rule: `+` moves the root, and it stays moved.
    #[test]
    fn re_rooting_is_what_moves_the_root_and_it_outlives_the_tree() {
        let d = ScratchDir::new("plus-sticks").file("pkg/sub/a.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Down);
        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Expand);
        tree_key(&mut ed, TreeCmd::Select { down: true, count: 1 });

        tree_key(&mut ed, TreeCmd::Enter);
        tree_key(&mut ed, TreeCmd::Up);

        let tree = ed.window().tree().expect("the tree is back");
        assert_eq!(
            tree.root(),
            std::path::Path::new(&format!("{}/pkg", d.path())),
            "where `+` put it, not where the file happens to live",
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
        assert_eq!(ed.session.options.number, LineNumbers::Every(1), "every line, by default");

        ex(&mut ed, "set number 0");
        assert_eq!(ed.session.options.number, LineNumbers::Off);
        ex(&mut ed, "set number -1");
        assert_eq!(ed.session.options.number, LineNumbers::Relative);
        ex(&mut ed, "set number 5");
        assert_eq!(ed.session.options.number, LineNumbers::Every(5));
        // Vim's spelling, which the fingers type without asking.
        ex(&mut ed, "set number=10");
        assert_eq!(ed.session.options.number, LineNumbers::Every(10));
    }

    #[test]
    fn set_reaches_every_option_the_config_file_can() {
        let mut ed = Editor::empty();

        ex(&mut ed, "set hlsearch true");
        assert!(ed.session.options.hlsearch, "`:set` reaches an option it never used to");

        ex(&mut ed, "set hlsearch");
        assert_eq!(ed.session.status, "hlsearch=true", "and reports it back");

        ex(&mut ed, "set hlsearch false");
        assert!(!ed.session.options.hlsearch);
    }

    #[test]
    fn set_reports_the_options_own_message_for_a_bad_value() {
        let mut ed = Editor::empty();

        ex(&mut ed, "set hlsearch maybe");
        assert_eq!(ed.session.status, "hlsearch takes true or false: maybe");

        ex(&mut ed, "set nmber 5");
        assert_eq!(ed.session.status, "unknown option: nmber");
    }

    #[test]
    fn set_reports_and_refuses_rather_than_guessing() {
        let mut ed = editor("one");
        ex(&mut ed, "set number 5");

        ex(&mut ed, "set number");
        assert_eq!(ed.session.status, "number=5", "no value asks rather than sets");

        ex(&mut ed, "set number -3");
        assert_eq!(ed.session.options.number, LineNumbers::Every(5), "left alone");
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

        // The new window opens on the right, so its neighbour is to the left.
        ed.apply(cmd(Action::Window(WindowCmd::Focus(Side::Left))));
        assert_ne!(ed.focus(), first);

        ed.apply(cmd(Action::Window(WindowCmd::Focus(Side::Right))));
        assert_eq!(ed.focus(), first, "and back");
    }

    #[test]
    fn there_is_nothing_past_the_edge_so_focus_stays_put() {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);
        let first = ed.focus();

        // Focus is in the new window, which is the rightmost one.
        ed.apply(cmd(Action::Window(WindowCmd::Focus(Side::Right))));
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

    /// The only window cannot close, so it falls to the next buffer instead —
    /// which is what `:bd` does in a session that never split.
    #[test]
    fn bd_in_the_only_window_falls_through_to_the_next_buffer() {
        let a = Scratch::new("del_a.txt", "a\n");
        let b = Scratch::new("del_b.txt", "b\n");
        let mut ed = opened(&a);
        ex(&mut ed, &format!("e {}", b.path()));

        ex(&mut ed, "bd");
        assert_eq!(ed.buffer_ids().len(), 1);
        assert_eq!(ed.window_ids().len(), 1, "and nothing to collapse");
        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "a\n",
            "the window fell to the next one"
        );
    }

    /// The panes showing a deleted buffer go with it, and the layout collapses
    /// to fill — three views of one file should not become three views of some
    /// other file nobody asked to see three times.
    #[test]
    fn bd_closes_the_windows_that_showed_the_buffer_and_collapses_the_splits() {
        let a = Scratch::new("split_del_a.txt", "a\n");
        let b = Scratch::new("split_del_b.txt", "b\n");
        let mut ed = opened(&a);

        // Three panes: two on `a`, one on `b`.
        split(&mut ed, Dir::Vertical);
        split(&mut ed, Dir::Horizontal);
        ex(&mut ed, &format!("e {}", b.path()));
        assert_eq!(ed.window_ids().len(), 3);
        let survivor = ed.focus();

        // `:bd` deletes the focused window's buffer, so move onto one of the
        // two panes showing `a` — which means the pane deleting it is one of
        // the panes that goes.
        while ed.window().buffer() != Some(ed.buffer_ids()[0]) {
            ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        }
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\n");
        ex(&mut ed, "bd");

        assert_eq!(ed.window_ids(), vec![survivor], "both panes on `a` closed");
        assert_eq!(ed.focus(), survivor, "and focus landed on the one on `b`");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "b\n");
        assert_eq!(ed.buffer_ids().len(), 1);
        // The collapse gives the space back: the vertical split had halved it.
        let rect = ed.layout.rect_of(survivor, ed.area, &ed.chrome).expect("laid out");
        assert_eq!(rect.width, 80, "and the survivor fills the screen again");
    }

    /// Something has to survive — the last window cannot close — and the pane
    /// the user is in is the one worth keeping.
    #[test]
    fn bd_keeps_the_focused_pane_when_every_pane_showed_the_buffer() {
        let a = Scratch::new("all_del_a.txt", "a\n");
        let b = Scratch::new("all_del_b.txt", "b\n");
        let mut ed = opened(&a);
        ex(&mut ed, &format!("e {}", b.path()));
        ex(&mut ed, "b all_del_a");

        split(&mut ed, Dir::Vertical);
        split(&mut ed, Dir::Horizontal);
        let focused = ed.focus();
        assert!(ed.window_ids().iter().all(|&w| ed.window_of(w).unwrap().buffer().is_some()));

        ex(&mut ed, "bd");

        assert_eq!(ed.window_ids(), vec![focused], "the focused pane is the one left");
        assert_eq!(ed.focus(), focused, "and focus never moved");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "b\n", "on the next buffer");
    }

    /// A tree shows no buffer, so it is never one of the panes that closes —
    /// and a session of nothing but a tree is already reachable with `bi .`.
    #[test]
    fn bd_beside_a_tree_closes_only_the_pane_that_showed_the_buffer() {
        let d = ScratchDir::new("bd-tree").file("a.rs");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        sized(&mut ed);
        ed.apply(cmd(Action::Window(WindowCmd::Tree)));
        assert_eq!(ed.window_ids().len(), 2);
        let tree = ed.focus();

        ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: false })));
        assert!(ed.window().buffer().is_some(), "focus is on the text pane");
        ex(&mut ed, "bd");

        assert_eq!(ed.window_ids(), vec![tree], "only the tree is left");
        assert!(ed.window().tree().is_some());
        assert_eq!(ed.buffer_ids().len(), 1, "and the list is never empty");
        assert!(ed.entry(ed.buffer_ids()[0]).buffer.path.is_none(), "a fresh no-name buffer");
    }

    /// The heir is made before the survivor can show it, and closing a window
    /// sweeps blanks nobody is looking at — so an heir made too early is swept
    /// out from under the `show` that was about to save it. Deleting the only
    /// buffer out of a split is the one path where both happen at once.
    #[test]
    fn bd_of_the_only_buffer_across_splits_leaves_a_fresh_one_to_land_on() {
        let a = Scratch::new("only_del.txt", "a\n");
        let mut ed = opened(&a);
        split(&mut ed, Dir::Vertical);
        split(&mut ed, Dir::Horizontal);
        let focused = ed.focus();

        ex(&mut ed, "bd");

        assert_eq!(ed.window_ids(), vec![focused]);
        assert_eq!(ed.buffer_ids().len(), 1);
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "");
        assert_eq!(ed.buffer().unwrap().path, None, "and it is a no-name buffer");
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

    fn visual(text: &str, at: usize, kind: Shape) -> Editor {
        let mut ed = editor(text);
        ed.set_cursor(Cursor::at(at));
        ed.apply(cmd(Action::EnterVisual(kind)));
        ed
    }

    #[test]
    fn v_starts_a_selection_and_motions_move_only_the_head() {
        let mut ed = visual("hello world", 0, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        let sel = ed.selections().unwrap().primary();
        assert_eq!(sel.anchor.at, 0, "the anchor stays where the selection began");
        assert_eq!(sel.head.at, 2);
    }

    #[test]
    fn the_same_key_again_leaves_visual_mode() {
        let mut ed = visual("hello", 0, Shape::Chars);
        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars));
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.selections().unwrap().primary().is_collapsed());
    }

    #[test]
    fn v_then_big_v_switches_kind_rather_than_leaving() {
        let mut ed = visual("hello", 0, Shape::Chars);
        ed.apply(cmd(Action::EnterVisual(Shape::Lines)));
        assert_eq!(ed.session.mode, Mode::Visual(Shape::Lines));
    }

    #[test]
    fn o_swaps_the_ends_so_the_other_one_can_be_adjusted() {
        let mut ed = visual("hello world", 2, Shape::Chars);
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
        let mut ed = visual("hello", 0, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "llo", "both h and e");
        assert_eq!(ed.session.mode, Mode::Normal, "and it drops back to normal");
    }

    #[test]
    fn a_linewise_operator_takes_whole_lines_whatever_the_columns() {
        let mut ed = visual("one\ntwo\nthree", 5, Shape::Lines);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Delete, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\nthree");
    }

    #[test]
    fn a_visual_change_leaves_you_in_insert_mode() {
        let mut ed = visual("hello", 0, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Change, sink: Sink::Ring }));
        assert_eq!(ed.session.mode, Mode::Insert);
    }

    #[test]
    fn a_visual_yank_captures_without_changing_the_text() {
        let mut ed = visual("hello", 0, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "hello");
        assert_eq!(ed.session.registers.front().unwrap().text, "he");
    }

    #[test]
    fn a_linewise_yank_is_a_linewise_entry() {
        let mut ed = visual("one\ntwo", 0, Shape::Lines);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        let entry = ed.session.registers.front().unwrap();
        assert_eq!(entry.kind, Shape::Lines);
        assert!(entry.text.ends_with('\n'), "or pasting it could not open a line");
    }

    #[test]
    fn viw_makes_the_object_the_selection() {
        let mut ed = visual("foo bar baz", 5, Shape::Chars);
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
        let mut ed = visual(text, at, Shape::Block);
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
        let mut ed = visual(GRID, 19, Shape::Block); // 'r', bottom right
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
        assert_eq!(entry.kind, Shape::Block);
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
        let mut ed = visual("abc\ndef", 1, Shape::Chars);
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
        let mut ed = visual("foo bar foo", 0, Shape::Chars);
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
        ed.apply(cmd(Action::EnterVisual(Shape::Block)));
        assert_eq!(ed.selections().unwrap().len(), 1);
    }

    #[test]
    fn pasting_a_block_puts_a_rectangle_back() {
        let mut ed = block(GRID, 1, 2, 1);
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));
        ed.set_cursor(Cursor::at(4)); // row 0, column 4
        ed.apply(cmd(Action::Paste { before: false, count: 1, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdebcf\nghijkhil\nmnopqnor");
    }

    #[test]
    fn pasting_a_block_pads_short_rows_and_grows_the_buffer() {
        let mut ed = editor("xy");
        ed.session.registers.push(Entry { text: "bc\nhi\nno".into(), kind: Shape::Block });
        ed.set_cursor(Cursor::at(1));
        ed.apply(cmd(Action::Paste { before: true, count: 1, sink: Sink::Ring }));
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

    // ---- paste over a selection --------------------------------------------

    /// Visual mode's `p` and `P`. See `docs/specs/registers.md`.
    fn paste_over(capture: bool, count: usize) -> Command {
        cmd(Action::PasteSelection { capture, count, sink: Sink::Ring })
    }

    /// A selection of `chars` characters from `at`, with `text` on the ring.
    fn ready(text: &str, at: usize, chars: usize, entry: Entry) -> Editor {
        let mut ed = visual(text, at, Shape::Chars);
        for _ in 1..chars {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }
        ed.session.registers.push(entry);
        ed
    }

    fn charwise(text: &str) -> Entry {
        Entry { text: text.into(), kind: Shape::Chars }
    }

    fn linewise(text: &str) -> Entry {
        Entry { text: text.into(), kind: Shape::Lines }
    }

    #[test]
    fn pasting_over_a_charwise_selection_replaces_it() {
        let mut ed = ready("one two", 4, 3, charwise("one"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one one");
        assert_eq!(heads(&ed), vec![6], "on the last char of what was pasted");
        assert_eq!(ed.session.mode, Mode::Normal, "and visual mode is over");
    }

    /// The register is read before the removal, or the paste would put back
    /// the text it had just taken out.
    #[test]
    fn the_paste_reads_the_register_the_selection_is_about_to_overwrite() {
        let mut ed = ready("one two", 4, 3, charwise("one"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one one", "not \"one two\"");
    }

    #[test]
    fn p_leaves_what_it_replaced_on_the_ring_and_capital_p_does_not() {
        let mut ed = ready("one two", 4, 3, charwise("one"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.session.registers.front().unwrap().text, "two", "ready to swap it in");

        let mut ed = ready("one two", 4, 3, charwise("one"));
        ed.apply(paste_over(false, 1));
        assert_eq!(ed.session.registers.front().unwrap().text, "one", "the ring is untouched");
    }

    /// A linewise entry is lines, and lines cannot sit inside one — so the
    /// line splits where the selection was.
    #[test]
    fn a_linewise_entry_over_a_charwise_selection_splits_the_line() {
        let mut ed = ready("one\ntwo three", 4, 3, linewise("one\n"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\n\none\n three");
        assert_eq!(heads(&ed), vec![5], "the first non-blank of the pasted line");
    }

    #[test]
    fn a_charwise_entry_over_a_linewise_selection_becomes_one_line() {
        let mut ed = visual("one two\nthree\nfour", 8, Shape::Lines);
        ed.session.registers.push(charwise("one"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one two\none\nfour");
        assert_eq!(heads(&ed), vec![8]);
    }

    #[test]
    fn a_linewise_entry_over_a_linewise_selection_replaces_the_lines() {
        let mut ed = visual("one\ntwo\nthree", 4, Shape::Lines);
        ed.session.registers.push(linewise("one\n"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\none\nthree");
        assert_eq!(ed.session.registers.front().unwrap().text, "two\n", "captured linewise");
    }

    /// A file that ended without a newline still does.
    #[test]
    fn pasting_over_the_last_line_invents_no_terminator() {
        let mut ed = visual("one\ntwo", 4, Shape::Lines);
        ed.session.registers.push(linewise("three\n"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\nthree");
    }

    #[test]
    fn a_count_pastes_several_copies_over_the_selection_in_one_undo_step() {
        let mut ed = ready("one two", 4, 3, charwise("ab"));
        ed.apply(paste_over(true, 3));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one ababab");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one two", "one step, not three");
    }

    /// Vim deletes here on the reasoning that `v_p` is a delete and a put. That
    /// stops being true the moment the register is empty.
    #[test]
    fn an_empty_ring_leaves_the_selection_alone() {
        let mut ed = visual("one two", 4, Shape::Chars);
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one two");
        assert_eq!(ed.session.status, "nothing to paste");
        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars), "still selecting");
    }

    #[test]
    fn the_black_hole_pastes_nothing_over_a_selection() {
        let mut ed = ready("one two", 4, 3, charwise("one"));
        ed.apply(cmd(Action::PasteSelection { capture: true, count: 1, sink: Sink::BlackHole }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one two", "nothing comes out");
    }

    /// The sink names where the paste comes *from*. What it displaced is
    /// ordinary editing history and belongs on the ring.
    #[test]
    fn a_clipboard_paste_over_a_selection_leaves_the_removed_text_on_the_ring() {
        let clipboard = FakeClipboard::default();
        *clipboard.0.borrow_mut() = Some("zzz".into());
        let mut ed = visual("one two", 4, Shape::Chars);
        ed.set_clipboard(clipboard.clone());
        for _ in 0..2 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }

        ed.apply(cmd(Action::PasteSelection { capture: true, count: 1, sink: Sink::System }));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one zzz");
        assert_eq!(ed.session.registers.front().unwrap().text, "two");
        assert_eq!(clipboard.0.borrow().as_deref(), Some("zzz"), "and the clipboard is unchanged");
    }

    #[test]
    fn a_blockwise_entry_over_a_charwise_selection_goes_in_as_a_rectangle() {
        let mut ed = ready("ab\ncd\nefgh", 6, 2, Entry { text: "a\nc".into(), kind: Shape::Block });
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "ab\ncd\nagh\nc");
    }

    #[test]
    fn a_charwise_entry_over_a_block_lands_on_every_row() {
        let mut ed = block("abc\ndef\nghi", 4, 1, 1);
        ed.session.registers.push(charwise("abc"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abc\nabcf\nabci");
        assert_eq!(
            ed.session.registers.front().unwrap().kind,
            Shape::Block,
            "and the rectangle it replaced comes back as one"
        );
    }

    #[test]
    fn a_linewise_entry_over_a_block_opens_lines_below_it() {
        let mut ed = block("abc\ndef\nghi", 4, 1, 1);
        ed.session.registers.push(linewise("abc\n"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abc\nf\ni\nabc");
    }

    #[test]
    fn a_blockwise_entry_over_a_block_replaces_the_rectangle() {
        let mut ed = block("abcd\nefgh\nijkl", 10, 0, 1);
        ed.session.registers.push(Entry { text: "ab\nef".into(), kind: Shape::Block });
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcd\nefgh\nabkl\nef");
    }

    /// `P` is the one that repeats usefully: `p` put what it displaced on the
    /// front of the ring, so repeating a swap swaps again.
    #[test]
    fn dot_repeats_a_visual_paste_over_the_same_extent() {
        let mut ed = ready("aa bb cc", 0, 2, charwise("zz"));
        ed.apply(paste_over(false, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "zz bb cc");

        ed.set_cursor(Cursor::at(3));
        ed.apply(cmd(Action::RepeatChange { count: None }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "zz zz cc", "two characters again");
    }

    #[test]
    fn the_register_picker_over_a_selection_replaces_it_too() {
        let mut ed = visual("one two", 4, Shape::Chars);
        ed.session.registers.push(charwise("one"));
        for _ in 0..2 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }
        ed.apply(cmd(Action::OpenPicker(PickerKind::Register { before: false })));
        ed.apply(cmd(Action::PickAccept));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one one");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn cancelling_the_picker_goes_back_to_the_selection_it_was_opened_from() {
        let mut ed = visual("one two", 4, Shape::Chars);
        ed.session.registers.push(charwise("one"));
        ed.apply(cmd(Action::OpenPicker(PickerKind::Register { before: false })));
        ed.apply(cmd(Action::PickCancel));
        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars));
    }

    // ---- named registers ---------------------------------------------------

    /// `"nyy` captures first and asks after: the prompt opens prefilled once
    /// the command is done, and `:yname a` files the capture under the name —
    /// in the named space, not on the ring.
    #[test]
    fn a_named_capture_prompts_and_the_name_stores_it() {
        let mut ed = editor("alpha\n");
        ed.set_cursor(Cursor::at(0));
        ed.apply(cmd(Action::Operate {
            op: Operator::Yank,
            target: Target::Motion(Motion::CurrentLine),
            count: 1,
            sink: Sink::Named,
        }));
        assert!(
            matches!(&ed.session.mode, Mode::Command(line) if line.to_string() == "yname "),
            "the prompt opened prefilled"
        );

        ed.apply(cmd(Action::CommandChar('a')));
        ed.apply(cmd(Action::CommandExecute));

        assert_eq!(ed.session.registers.named_at(0).unwrap().text, "alpha\n");
        assert!(ed.session.registers.is_empty(), "the named space is not the ring");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    /// Backing out of the prompt keeps the text — on the ring, where an
    /// unnamed capture would have gone. `"ndd` then Esc must not be a way to
    /// lose a line.
    #[test]
    fn abandoning_the_name_prompt_keeps_the_capture_on_the_ring() {
        let mut ed = editor("alpha\n");
        ed.set_cursor(Cursor::at(0));
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::CurrentLine),
            count: 1,
            sink: Sink::Named,
        }));
        ed.apply(cmd(Action::CommandCancel));

        assert_eq!(ed.session.registers.front().unwrap().text, "alpha\n", "not lost");
        assert!(ed.session.registers.named().is_empty());
    }

    /// `"np` is a choice, not a slot: the picker over the names opens, and
    /// the chosen entry pastes and leads the ring so `.` and `p` repeat it.
    #[test]
    fn quote_n_p_pastes_a_named_register_through_the_picker() {
        let mut ed = editor("start ");
        ed.session.registers.set_named("word", charwise("kept"));
        ed.session.registers.push(charwise("noise"));
        ed.set_cursor(Cursor::at(5));

        ed.apply(cmd(Action::Paste { before: false, count: 1, sink: Sink::Named }));
        assert!(ed.session.picker.is_some(), "the picker over the names opened");
        ed.apply(cmd(Action::PickAccept));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "start kept");
        assert_eq!(ed.session.registers.front().unwrap().text, "kept", "leads the ring");
    }

    #[test]
    fn quote_n_p_with_no_names_says_so() {
        let mut ed = editor("x");
        ed.apply(cmd(Action::Paste { before: false, count: 1, sink: Sink::Named }));
        assert!(ed.session.picker.is_none());
        assert_eq!(ed.session.status, "no named registers");
    }

    /// `:yname` takes a range, the way `:d` does: the region goes straight
    /// into the named space — the name is already on the line, so there is
    /// nothing to prompt for.
    #[test]
    fn yname_with_a_range_yanks_the_lines_into_the_name() {
        let mut ed = editor("one\ntwo\nthree\n");
        ed.run_ex("1,2yname a");

        let entry = ed.session.registers.named_at(0).unwrap();
        assert_eq!(entry.text, "one\ntwo\n");
        assert_eq!(entry.kind, Shape::Lines);
        assert!(ed.session.registers.is_empty(), "the ring is not involved");
    }

    /// No range and nothing held is the cursor's line — the same fallback
    /// every line-scoped `:` command answers with.
    #[test]
    fn a_bare_yname_with_nothing_held_yanks_the_cursor_line() {
        let mut ed = editor("one\ntwo\n");
        ed.set_cursor(Cursor::at(0));
        ed.run_ex("yname a");

        let entry = ed.session.registers.named_at(0).unwrap();
        assert_eq!(entry.text, "one\n");
        assert_eq!(entry.kind, Shape::Lines);
    }

    /// Over a selection — the `'v` scope the `:` line prefills from visual —
    /// it takes the selection with its own shape, so what was charwise
    /// pastes back inline.
    #[test]
    fn yname_over_a_selection_keeps_its_shape() {
        let mut ed = visual("one two", 4, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.run_ex("'vyname a");

        let entry = ed.session.registers.named_at(0).unwrap();
        assert_eq!(entry.kind, Shape::Chars);
        assert_eq!(entry.text, "two");
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

    /// The point of skipping: an occurrence you do not want to edit is passed
    /// over, and the cursor count does not grow.
    #[test]
    fn skip_moves_the_cursor_on_instead_of_leaving_one_behind() {
        let mut ed = editor("foo foo foo");
        ed.apply(cmd(Action::SkipCursorToNextMatch));

        assert_eq!(ed.selections().unwrap().len(), 1, "skipping never adds a cursor");
        assert_eq!(heads(&ed), vec![4], "moved to the second occurrence");

        ed.apply(cmd(Action::SkipCursorToNextMatch));
        assert_eq!(heads(&ed), vec![8], "and on to the third");
    }

    /// Skipping in the middle of a multi-cursor edit has to leave the cursors
    /// already placed alone — it is the newest one, the one just landed on a
    /// match, that the user is rejecting.
    #[test]
    fn skip_replaces_only_the_newest_cursor() {
        let mut ed = editor("foo foo foo foo");
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(heads(&ed), vec![0, 4]);

        ed.apply(cmd(Action::SkipCursorToNextMatch));
        assert_eq!(heads(&ed), vec![0, 8], "the second cursor moved, the first stayed");

        // And adding again resumes from where the skip left off.
        ed.apply(cmd(Action::AddCursorNextMatch));
        assert_eq!(heads(&ed), vec![0, 8, 12]);
    }

    #[test]
    fn skip_with_nowhere_to_go_reports_and_does_not_move() {
        let mut ed = editor("unique word");
        ed.apply(cmd(Action::SkipCursorToNextMatch));
        assert_eq!(ed.selections().unwrap().len(), 1);
        assert_eq!(heads(&ed), vec![0], "stayed put");
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
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
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
            target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "two three");

        // Neither of these is a change.
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::Operate {
            op: Operator::Yank,
            target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
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
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
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
        ed.apply(cmd(Action::EnterVisual(Shape::Lines)));
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
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
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
            !ed.session.options.hlsearch,
            "a plain `/` does not light the buffer up, as in vim"
        );

        ed.run_ex("hls");
        assert!(ed.session.options.hlsearch);
        ed.run_ex("noh");
        assert!(!ed.session.options.hlsearch);
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

    /// The horizontal mirror of the scrolloff clamp: the cursor walks off the
    /// right edge and the window slides after it, margin included; walking
    /// back to the start brings the window home. `:zen` first, so the text
    /// width is the window width and the numbers are the test's own.
    #[test]
    fn the_window_follows_the_cursor_sideways() {
        let mut ed = editor(&format!("{}\n", "x".repeat(100)));
        ed.run_ex("zen");
        let focus = ed.focus();
        ed.set_cursor(Cursor::at(0));
        ed.size_window(focus, 20, 5);
        assert_eq!(text_of(&ed, focus).left, 0);

        ed.set_cursor(Cursor::at(99));
        ed.size_window(focus, 20, 5);
        assert_eq!(text_of(&ed, focus).left, 99 + 3 + 1 - 20, "the margin past the right edge");

        ed.set_cursor(Cursor::at(0));
        ed.size_window(focus, 20, 5);
        assert_eq!(text_of(&ed, focus).left, 0, "and back");
    }

    /// The offset is display columns: fifty CJK chars are a hundred cells,
    /// and the window scrolls by what the screen shows rather than by chars.
    #[test]
    fn sideways_scrolling_counts_cells_not_chars() {
        let mut ed = editor(&format!("{}\n", "漢".repeat(50)));
        ed.run_ex("zen");
        let focus = ed.focus();
        ed.set_cursor(Cursor::at(49));
        ed.size_window(focus, 20, 5);
        assert_eq!(text_of(&ed, focus).left, 98 + 3 + 1 - 20, "char 49 sits at cell 98");
    }

    /// A width nobody reported — a headless embedder, a test that never drew —
    /// must not pin the window to the cursor.
    #[test]
    fn no_reported_width_means_no_sideways_scrolling() {
        let mut ed = editor(&format!("{}\n", "x".repeat(100)));
        let focus = ed.focus();
        ed.set_cursor(Cursor::at(99));
        ed.scroll_to_cursor(5);
        assert_eq!(text_of(&ed, focus).left, 0);
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

    // ---- indentation --------------------------------------------------------
    //
    // The buffer's own tests say what an indent *is*; these say that the
    // commands reach it, that the cursor and the selection end up where they
    // should, and that one `>` is one undo step.

    fn indent(target: Target, count: usize, right: bool) -> Command {
        cmd(Action::Operate { op: Operator::Indent { right }, target, count, sink: Sink::Ring })
    }

    #[test]
    fn shift_right_moves_the_line_and_lands_on_its_first_non_blank() {
        let mut ed = editor("alpha\nbeta\n");

        ed.apply(indent(Target::Motion(Motion::CurrentLine), 1, true));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "    alpha\nbeta\n");
        assert_eq!(ed.cursor_col(), Some(4));
    }

    #[test]
    fn a_count_is_lines_in_normal_mode() {
        let mut ed = editor("a\nb\nc\nd\n");

        ed.apply(indent(Target::Motion(Motion::CurrentLine), 3, true));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "    a\n    b\n    c\nd\n");
    }

    #[test]
    fn indent_takes_a_text_object() {
        let mut ed = editor("a\nb\n\nc\n");

        ed.apply(indent(Target::Object { object: TextObject::Paragraph, around: false }, 1, true));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "    a\n    b\n\nc\n");
    }

    #[test]
    fn indent_is_one_undo_step_however_many_lines_it_touched() {
        let mut ed = editor("a\nb\nc\n");
        ed.apply(indent(Target::Motion(Motion::CurrentLine), 3, true));

        ed.apply(cmd(Action::Undo));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "a\nb\nc\n");
    }

    #[test]
    fn dot_repeats_an_indent() {
        let mut ed = editor("alpha\n");
        ed.apply(indent(Target::Motion(Motion::CurrentLine), 1, true));

        ed.apply(cmd(Action::RepeatChange { count: None }));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "        alpha\n");
    }

    /// The count is steps here, and the selection survives — which is what
    /// lets three steps be the command three times.
    #[test]
    fn a_visual_indent_takes_steps_and_keeps_the_selection() {
        let mut ed = editor("alpha\nbeta\n");
        ed.apply(cmd(Action::EnterVisual(Shape::Lines)));
        ed.apply(cmd(Action::Move(Motion::Down)));

        ed.apply(Command {
            count: 3,
            action: Action::OperateSelection {
                op: Operator::Indent { right: true },
                sink: Sink::Ring,
            },
        });

        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "            alpha\n            beta\n"
        );
        assert_eq!(ed.session.mode, Mode::Visual(Shape::Lines), "still selecting");
        let selection = ed.selections().unwrap().primary();
        let buffer = ed.buffer().unwrap();
        assert_eq!(buffer.row_at(Cursor::at(selection.range().0)), 0);
        assert_eq!(buffer.row_at(Cursor::at(selection.range().1)), 1, "both rows, still");
    }

    #[test]
    fn tab_in_insert_mode_reaches_the_next_stop() {
        let mut ed = editor("ab\n");
        ed.apply(cmd(Action::EnterInsertLineEnd));

        ed.apply(cmd(Action::InsertIndent { right: true }));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "ab  \n", "two columns, not four");
    }

    #[test]
    fn opening_a_line_carries_the_indent_and_esc_takes_back_an_unused_one() {
        let mut ed = editor("    alpha\n");
        ed.apply(cmd(Action::OpenLineBelow));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "    alpha\n    \n");

        ed.apply(cmd(Action::EnterNormal));

        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "    alpha\n\n",
            "an indent nothing was typed into is not left behind"
        );
    }

    #[test]
    fn autoindent_off_leaves_both_halves_of_that_alone() {
        let mut ed = editor("    alpha\n");
        // Through `:set` rather than by poking the session: options resolve
        // per buffer now, and a field written behind the resolver's back would
        // not reach the buffer that is already open.
        ex(&mut ed, "set autoindent false");

        ed.apply(cmd(Action::OpenLineBelow));
        type_str(&mut ed, "  ");
        ed.apply(cmd(Action::EnterNormal));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "    alpha\n  \n");
    }

    // ---- options, per file --------------------------------------------------
    //
    // See `docs/specs/options.md`. A file type is a *name*, so these need real
    // ones on disk — `Scratch` prefixes what it is given, and a Makefile is
    // only a Makefile if it is called one.

    /// A directory of files with the names they were asked for.
    struct Files(std::path::PathBuf);

    impl Files {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("bi-options-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn dir(&self, name: &str) -> std::path::PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        fn file(&self, name: &str, text: &str) -> String {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            path.to_str().unwrap().to_string()
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_makefile_gets_tabs_whatever_the_config_says() {
        let files = Files::new("make");
        let mut ed = Editor::open(files.file("Makefile", "all:\n\techo hi\n")).unwrap();
        assert!(!ed.options().expandtab, "out of the box");
        assert_eq!(ed.options().tab_width, 8);

        ed.load_config(ConfigText(Some("[options]\nexpandtab = true\n")));

        assert!(ed.session.options.expandtab, "the session wanted spaces");
        assert!(!ed.options().expandtab, "and a Makefile still does not get them");
    }

    #[test]
    fn a_filetype_section_beats_the_built_in_table() {
        let files = Files::new("ftsection");
        let mut ed = Editor::open(files.file("Makefile", "all:\n")).unwrap();

        ed.load_config(ConfigText(Some("[filetype.make]\ntab_width = 2\n")));

        assert_eq!(ed.options().tab_width, 2, "yours, over bi's own 8");
        assert!(!ed.options().expandtab, "and the rest of the built-in still stands");
    }

    #[test]
    fn set_beats_every_layer_under_it_and_reaches_open_buffers() {
        let files = Files::new("setwins");
        let mut ed = Editor::open(files.file("Makefile", "all:\n")).unwrap();
        ed.load_config(ConfigText(Some("[filetype.make]\ntab_width = 2\n")));

        ex(&mut ed, "set tab_width 5");

        assert_eq!(ed.options().tab_width, 5, "on the file that was already open");
    }

    #[test]
    fn two_kinds_of_file_open_at_once_resolve_differently() {
        let files = Files::new("both");
        let makefile = files.file("Makefile", "all:\n");
        let script = files.file("run.py", "print(1)\n");

        let mut ed = Editor::open(&makefile).unwrap();
        let python = ed.open_path(&script).unwrap();

        assert!(!ed.options().expandtab, "the Makefile keeps its tabs");
        assert!(ed.options_of(python).expandtab, "and the Python file does not get them");
    }

    #[test]
    fn a_file_nothing_claims_gets_the_session_options() {
        let files = Files::new("plain");
        let mut ed = Editor::open(files.file("notes.unknownext", "hi\n")).unwrap();
        ed.load_config(ConfigText(Some("[options]\ntab_width = 3\n")));

        assert_eq!(ed.options().tab_width, 3);
    }

    #[test]
    fn reload_re_resolves_every_open_buffer() {
        let files = Files::new("reload");
        let mut ed = Editor::open(files.file("Makefile", "all:\n")).unwrap();
        let source = std::rc::Rc::new(Mutable::new("[filetype.make]\ntab_width = 2\n"));
        ed.load_config(std::rc::Rc::clone(&source));
        assert_eq!(ed.options().tab_width, 2);

        *source.0.borrow_mut() = Some(String::new());
        ex(&mut ed, "reload");

        assert_eq!(ed.options().tab_width, 8, "back to what a Makefile asks for");
    }

    #[test]
    fn writing_a_buffer_under_a_new_name_re_resolves_it() {
        let files = Files::new("rename");
        let mut ed = Editor::open(files.file("notes.txt", "hi\n")).unwrap();
        assert!(ed.options().expandtab);

        ex(&mut ed, &format!("w {}", files.0.join("Makefile").display()));

        assert!(!ed.options().expandtab, "it is a Makefile now, and Makefiles take tabs");
    }

    #[test]
    fn a_bad_value_in_a_filetype_section_is_reported_rather_than_fatal() {
        let files = Files::new("badvalue");
        let mut ed = Editor::open(files.file("Makefile", "all:\n")).unwrap();

        let problems = ed.load_config(ConfigText(Some("[filetype.make]\ntab_width = \"wide\"\n")));

        assert_eq!(problems.len(), 1, "reported, with the line it is on");
        assert_eq!(problems[0].line, 2);
        assert_eq!(ed.options().tab_width, 8, "and the bad line changed nothing");
    }

    #[test]
    fn a_project_that_says_how_it_is_indented_is_believed() {
        let files = Files::new("editorconfig");
        // `root = true` so the walk stops here rather than wandering up into
        // whatever /tmp's parents have to say.
        files.file(".editorconfig", "root = true\n[*.py]\nindent_style = tab\nindent_size = 3\n");
        let mut ed = Editor::open(files.file("main.py", "x = 1\n")).unwrap();

        assert!(!ed.options().expandtab, "the project asked for tabs");
        assert_eq!(ed.options().shiftwidth, 3);
        assert_eq!(ed.options().tab_width, 3, "indent_size sets the width too");

        // Above the config's own [filetype.python]...
        ed.load_config(ConfigText(Some("[filetype.python]\nshiftwidth = 7\n")));
        assert_eq!(ed.options().shiftwidth, 3, "the project outranks your preference");

        // ...and below what you say out loud.
        ex(&mut ed, "set shiftwidth 5");
        assert_eq!(ed.options().shiftwidth, 5);
    }

    #[test]
    fn the_editorconfig_beside_the_file_beats_the_one_above_it() {
        let files = Files::new("editorconfig-nested");
        files.file(".editorconfig", "root = true\n[*]\nindent_size = 8\n");
        let inner = files.dir("src");
        std::fs::write(inner.join(".editorconfig"), "[*]\nindent_size = 2\n").unwrap();
        std::fs::write(inner.join("main.py"), "x = 1\n").unwrap();

        let ed = Editor::open(inner.join("main.py").to_str().unwrap()).unwrap();

        assert_eq!(ed.options().shiftwidth, 2);
    }

    #[test]
    fn reload_picks_up_an_edited_editorconfig() {
        let files = Files::new("editorconfig-reload");
        files.file(".editorconfig", "root = true\n[*]\nindent_size = 8\n");
        let mut ed = Editor::open(files.file("main.py", "x = 1\n")).unwrap();
        assert_eq!(ed.options().shiftwidth, 8);

        files.file(".editorconfig", "root = true\n[*]\nindent_size = 2\n");
        ed.load_config(ConfigText(None));

        assert_eq!(ed.options().shiftwidth, 2, "nothing is cached, so nothing goes stale");
    }

    // ---- trimming on write --------------------------------------------------

    #[test]
    fn a_write_takes_the_trailing_whitespace_with_it() {
        let f = Scratch::new("trim.rs", "alpha  \n\nbeta\t\n");
        let mut ed = opened(&f);

        ex(&mut ed, "w");

        assert_eq!(f.read(), "alpha\n\nbeta\n");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), f.read(), "and the buffer agrees");
    }

    #[test]
    fn a_trim_is_its_own_undo_step() {
        let f = Scratch::new("trim-undo.rs", "alpha  \n");
        let mut ed = opened(&f);
        ex(&mut ed, "w");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "alpha\n");

        ed.apply(cmd(Action::Undo));

        assert_eq!(
            ed.buffer().unwrap().rope().to_string(),
            "alpha  \n",
            "one `u` puts the whitespace back and nothing else"
        );
    }

    #[test]
    fn the_cursor_follows_the_text_across_a_trim() {
        let f = Scratch::new("trim-cursor.rs", "\n\nalpha\nbeta\n");
        let mut ed = opened(&f);
        // On the `b` of beta, which the two blank lines above are about to
        // pull two rows up.
        ed.apply(Command { count: 1, action: Action::Move(Motion::Line(4)) });
        assert_eq!(ed.cursor_row(), Some(3));

        ex(&mut ed, "w");

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "alpha\nbeta\n");
        assert_eq!(ed.cursor_row(), Some(1), "still on beta, rather than two rows past it");
    }

    /// The case that started this: a `.toml` with a few stray blank lines at
    /// the end of it, and a write that is supposed to take them.
    #[test]
    fn a_write_takes_the_blank_lines_off_the_end() {
        let f = Scratch::new("trim-last.toml", "edition = \"2024\"\n\n\n\n");
        let mut ed = opened(&f);

        ex(&mut ed, "w");

        assert_eq!(f.read(), "edition = \"2024\"\n");
    }

    #[test]
    fn trim_on_write_off_means_none_of_it() {
        let f = Scratch::new("trim-off.rs", "\nalpha  \n");
        let mut ed = opened(&f);

        ex(&mut ed, "set trim_on_write false");
        ex(&mut ed, "w");

        assert_eq!(f.read(), "\nalpha  \n");
    }

    #[test]
    fn markdown_keeps_the_two_spaces_that_mean_a_line_break() {
        let f = Scratch::new("notes.md", "\na line  \nand another\n");
        let mut ed = opened(&f);

        ex(&mut ed, "w");

        assert_eq!(f.read(), "a line  \nand another\n", "the break survives");
        assert!(!ed.options().trim.trailing);
        assert!(ed.options().trim.first_line, "but the blank first line still went");
    }

    #[test]
    fn a_filetype_section_can_disagree_with_bi_about_markdown() {
        let f = Scratch::new("opinion.md", "a line  \n");
        let mut ed = opened(&f);

        ed.load_config(ConfigText(Some("[filetype.markdown]\ntrim_trailing = true\n")));
        ex(&mut ed, "w");

        assert_eq!(f.read(), "a line\n");
    }

    #[test]
    fn a_project_can_ask_for_the_final_newline() {
        let files = Files::new("trim-editorconfig");
        files.file(".editorconfig", "root = true\n[*]\ninsert_final_newline = true\n");
        let path = files.file("main.py", "x = 1");
        let mut ed = Editor::open(&path).unwrap();

        ex(&mut ed, "w");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x = 1\n");
    }

    #[test]
    fn writing_every_buffer_trims_the_ones_nobody_is_looking_at() {
        let f = Scratch::new("trim-all.rs", "alpha  \n");
        let mut ed = opened(&f);
        ed.apply(cmd(Action::InsertChar('x')));
        ed.apply(cmd(Action::EnterNormal));

        ex(&mut ed, "wa");

        assert_eq!(f.read(), "xalpha\n");
    }

    // ---- :find and :replace --------------------------------------------------

    /// A project with three files, two of which mention `needle`.
    fn project(tag: &str) -> Files {
        let files = Files::new(tag);
        files.file("a.rs", "let needle = 1;\nlet other = 2;\n");
        files.file("b.rs", "// needle here\n// and needle again\n");
        files.file("c.rs", "nothing\n");
        files
    }

    /// The rows the results pane is showing, as text.
    fn result_rows(ed: &Editor) -> Vec<String> {
        use crate::results::Row;
        let results = ed.window().results().expect("a results pane");
        results
            .rows()
            .iter()
            .map(|row| match row {
                Row::File { path, matches } => format!("{} ({matches})", path.display()),
                Row::Hit { index } => {
                    let m = &results.matches()[*index];
                    format!("  {}: {}", m.line, m.text)
                }
            })
            .collect()
    }

    /// An editor rooted at `files`, showing one of them.
    fn in_project(files: &Files, name: &str) -> Editor {
        let path = files.0.join(name);
        let mut ed = Editor::open(&path).unwrap();
        ed.session.tree_root = Some(files.0.clone());
        ed
    }

    #[test]
    fn find_puts_what_it_found_in_a_pane() {
        let files = project("find-basic");
        let mut ed = in_project(&files, "c.rs");

        ex(&mut ed, "find needle");

        let mut rows = result_rows(&ed);
        // The walk's file order is not promised, so compare as a set of groups.
        rows.sort();
        assert_eq!(
            rows,
            [
                "  1: // needle here".to_string(),
                "  1: let needle = 1;".to_string(),
                "  2: // and needle again".to_string(),
                "a.rs (1)".to_string(),
                "b.rs (2)".to_string(),
            ]
        );
        assert_eq!(ed.session.status, "3 matches in 2 files");
    }

    #[test]
    fn finding_nothing_leaves_the_pane_you_were_in() {
        let files = project("find-nothing");
        let mut ed = in_project(&files, "c.rs");

        ex(&mut ed, "find haystack");

        assert!(ed.window().results().is_none(), "an empty pane is a worse answer than a line");
        assert_eq!(ed.session.status, "no matches for haystack");
        assert!(ed.buffer().is_some(), "and the file is still here");
    }

    #[test]
    fn a_find_pattern_is_literal_until_you_ask_otherwise() {
        let files = Files::new("find-literal");
        files.file("a.txt", "a.c\nabc\n");
        let mut ed = in_project(&files, "a.txt");

        ex(&mut ed, "find a.c");
        assert_eq!(ed.window().results().unwrap().matches().len(), 1, "`.` is a dot");

        ex(&mut ed, "find~ a.c");
        assert_eq!(ed.window().results().unwrap().matches().len(), 2, "and now it is a regex");
    }

    #[test]
    fn enter_on_a_row_opens_the_file_at_the_match() {
        let files = project("find-open");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find other");

        ed.apply(cmd(Action::Results(ResultsCmd::Move(1))));
        ed.apply(cmd(Action::Results(ResultsCmd::Open)));

        assert_eq!(ed.buffer().unwrap().line(1), "let other = 2;");
        let at = ed.cursor().unwrap();
        assert_eq!(ed.buffer().unwrap().row_at(at), 1);
        assert_eq!(ed.buffer().unwrap().col_at(at), 4, "on the match, not the start of the line");
    }

    #[test]
    fn enter_on_a_heading_opens_the_top_of_that_file() {
        let files = project("find-heading");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find other");

        // Row 0 is the heading, which is where the selection starts.
        ed.apply(cmd(Action::Results(ResultsCmd::Open)));

        assert_eq!(ed.buffer().unwrap().row_at(ed.cursor().unwrap()), 0);
    }

    #[test]
    fn the_pane_puts_back_what_it_displaced() {
        let files = project("find-close");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");
        assert!(ed.window().results().is_some());

        ed.apply(cmd(Action::Results(ResultsCmd::Close)));

        assert_eq!(ed.buffer().unwrap().line(0), "nothing", "the file is back");
    }

    #[test]
    fn replace_arms_first_and_a_capital_a_takes_everything() {
        let files = project("replace-basic");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");

        ex(&mut ed, "replace //pin/");
        assert!(
            ed.session.status.starts_with("3 rewrites offered"),
            "armed, nothing applied: {}",
            ed.session.status
        );
        assert!(
            std::fs::read_to_string(files.0.join("a.rs")).unwrap().contains("needle"),
            "arming rewrites nothing"
        );

        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));

        assert!(ed.session.status.starts_with("3 replaced in 2 files"), "{}", ed.session.status);
        // Unwritten: the files on disk still say what they said.
        assert!(std::fs::read_to_string(files.0.join("a.rs")).unwrap().contains("needle"));
        // And the buffers say the new thing.
        let a = ed.buffers.iter().find(|b| b.buffer.path.as_deref() == Some(&files.0.join("a.rs")));
        assert_eq!(a.unwrap().buffer.line(0), "let pin = 1;");
    }

    #[test]
    fn replace_commits_on_write_and_undoes_per_file() {
        let files = project("replace-write");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");
        ex(&mut ed, "replace //pin/");
        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));

        ex(&mut ed, "wa");

        assert_eq!(
            std::fs::read_to_string(files.0.join("a.rs")).unwrap(),
            "let pin = 1;\nlet other = 2;\n"
        );
        assert_eq!(
            std::fs::read_to_string(files.0.join("b.rs")).unwrap(),
            "// pin here\n// and pin again\n"
        );
    }

    #[test]
    fn replace_takes_both_matches_on_one_line() {
        // The pane shows one row per line, so a row that then replaced half of
        // itself would have lied about what it was offering.
        let files = Files::new("replace-twice");
        files.file("a.txt", "needle and needle\n");
        let mut ed = in_project(&files, "a.txt");
        ex(&mut ed, "find needle");

        ex(&mut ed, "replace //pin/");
        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));

        let a =
            ed.buffers.iter().find(|b| b.buffer.path.as_deref() == Some(&files.0.join("a.txt")));
        assert_eq!(a.unwrap().buffer.line(0), "pin and pin");
        assert!(ed.session.status.starts_with("2 replaced in 1 file"), "{}", ed.session.status);
    }

    #[test]
    fn replace_skips_a_line_that_has_moved_on_since_the_search() {
        let files = project("replace-stale");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");

        // The file changes underneath — which is what happens when you edit it
        // yourself between the search and the replace.
        std::fs::write(files.0.join("a.rs"), "something else entirely\n").unwrap();

        ex(&mut ed, "replace //pin/");
        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));

        assert!(
            ed.session.status.contains("changed since the search"),
            "never silent: {}",
            ed.session.status
        );
    }

    #[test]
    fn replace_with_no_results_says_where_to_start() {
        let files = project("replace-none");
        let mut ed = in_project(&files, "c.rs");

        ex(&mut ed, "replace //pin/");

        assert_eq!(ed.session.status, "no results here — `:find` first, or `:replace /old/new/`");
    }

    #[test]
    fn replace_without_a_delimiter_shows_the_shape() {
        let files = project("replace-shape");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");

        ex(&mut ed, "replace pin");

        assert_eq!(
            ed.session.status,
            "replace how? `:replace /old/new/` — `//new/` takes the pane's own search"
        );
        assert!(
            std::fs::read_to_string(files.0.join("a.rs")).unwrap().contains("needle"),
            "and nothing was rewritten"
        );
    }

    #[test]
    fn replace_with_a_pattern_is_find_and_the_offer_in_one_line() {
        let files = project("replace-standalone");
        let mut ed = in_project(&files, "c.rs");

        ex(&mut ed, "replace /needle/pin/");
        assert!(ed.session.status.starts_with("3 rewrites in 2 files"), "{}", ed.session.status);
        assert!(ed.window().results().is_some(), "the offer is a pane");

        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));
        assert!(ed.session.status.starts_with("3 replaced in 2 files"), "{}", ed.session.status);
    }

    #[test]
    fn replace_glued_to_its_argument_still_reads() {
        let files = project("replace-glued");
        let mut ed = in_project(&files, "c.rs");

        ex(&mut ed, "replace/needle/pin/");

        assert!(ed.window().results().is_some(), "{}", ed.session.status);
    }

    #[test]
    fn a_applies_the_selected_row_and_walks_on() {
        let files = Files::new("replace-one-row");
        files.file("a.rs", "one needle\ntwo needle\n");
        let mut ed = in_project(&files, "a.rs");
        ex(&mut ed, "find needle");
        ex(&mut ed, "replace //pin/");

        // Row 0 is the heading; row 1 the first hit.
        ed.apply(cmd(Action::Results(ResultsCmd::Move(1))));
        ed.apply(cmd(Action::Results(ResultsCmd::Apply)));

        assert!(ed.session.status.starts_with("1 replaced in 1 file"), "{}", ed.session.status);
        let a = ed.buffers.iter().find(|b| b.buffer.path.as_deref() == Some(&files.0.join("a.rs")));
        let buffer = &a.unwrap().buffer;
        assert_eq!(buffer.line(0), "one pin");
        assert_eq!(buffer.line(1), "two needle", "only the row you pressed `a` on");

        let results = ed.window().results().unwrap();
        assert!(results.is_applied(0), "the ✓ is the record");
        assert!(!results.is_applied(1));
        assert_eq!(results.selected(), 2, "and the selection walked to the next offer");
    }

    #[test]
    fn a_on_a_heading_takes_the_whole_file() {
        let files = project("replace-heading");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");
        ex(&mut ed, "replace //pin/");

        // Row 0 is the first file's heading.
        ed.apply(cmd(Action::Results(ResultsCmd::Apply)));

        assert!(ed.session.status.contains("replaced in 1 file"), "{}", ed.session.status);
    }

    #[test]
    fn a_with_nothing_armed_says_what_arms() {
        let files = project("replace-unarmed");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");

        ed.apply(cmd(Action::Results(ResultsCmd::Apply)));

        assert_eq!(ed.session.status, "nothing armed — `:replace //new/` offers a rewrite first");
    }

    #[test]
    fn replace_tilde_interpolates_its_groups() {
        let files = Files::new("replace-groups");
        files.file("a.rs", "fn alpha() {}\n");
        let mut ed = in_project(&files, "a.rs");

        ex(&mut ed, r"replace~ /fn (\w+)/fn new_$1/");
        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));

        let a = ed.buffers.iter().find(|b| b.buffer.path.as_deref() == Some(&files.0.join("a.rs")));
        assert_eq!(a.unwrap().buffer.line(0), "fn new_alpha() {}");
    }

    #[test]
    fn a_literal_replacement_keeps_its_dollars() {
        let files = Files::new("replace-dollars");
        files.file("a.rs", "the needle\n");
        let mut ed = in_project(&files, "a.rs");

        ex(&mut ed, "replace /needle/$1/");
        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));

        let a = ed.buffers.iter().find(|b| b.buffer.path.as_deref() == Some(&files.0.join("a.rs")));
        assert_eq!(a.unwrap().buffer.line(0), "the $1", "no groups you could not have written");
    }

    #[test]
    fn x_drops_a_row_and_the_replace_no_longer_offers_it() {
        let files = Files::new("prune-row");
        files.file("a.rs", "one needle\ntwo needle\n");
        let mut ed = in_project(&files, "a.rs");
        ex(&mut ed, "find needle");

        ed.apply(cmd(Action::Results(ResultsCmd::Move(1))));
        ed.apply(cmd(Action::Results(ResultsCmd::Remove)));
        assert_eq!(result_rows(&ed), ["a.rs (1)".to_string(), "  2: two needle".to_string()]);

        ex(&mut ed, "replace //pin/");
        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));

        let a = ed.buffers.iter().find(|b| b.buffer.path.as_deref() == Some(&files.0.join("a.rs")));
        let buffer = &a.unwrap().buffer;
        assert_eq!(buffer.line(0), "one needle", "dropped, so untouched");
        assert_eq!(buffer.line(1), "two pin");
    }

    #[test]
    fn x_on_a_heading_drops_the_file_and_on_the_last_row_closes_the_pane() {
        let files = project("prune-file");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");

        // Row 0: the first file's heading. Dropping it leaves the other file.
        ed.apply(cmd(Action::Results(ResultsCmd::Remove)));
        assert_eq!(ed.window().results().unwrap().files(), 1);

        ed.apply(cmd(Action::Results(ResultsCmd::Remove)));
        assert!(ed.window().results().is_none(), "an emptied list closes");
        assert_eq!(ed.session.status, "nothing left in the list");
        assert_eq!(ed.buffer().unwrap().line(0), "nothing", "the file is back");
    }

    #[test]
    fn find_scoped_to_a_directory_walks_only_it() {
        let files = Files::new("find-scoped");
        files.dir("sub");
        files.file("top.rs", "needle up here\n");
        files.file("sub/inner.rs", "needle down here\n");
        let mut ed = in_project(&files, "top.rs");

        ex(&mut ed, "find sub/ needle");

        assert_eq!(
            result_rows(&ed),
            ["sub/inner.rs (1)".to_string(), "  1: needle down here".to_string()],
            "the scope narrows the walk, not the names"
        );

        // A first word naming no directory stays in the pattern.
        ex(&mut ed, "find zub/ needle");
        assert_eq!(ed.session.status, "no matches for zub/ needle");
    }

    #[test]
    fn replace_scoped_to_a_directory_offers_only_it() {
        let files = Files::new("replace-scoped");
        files.dir("sub");
        files.file("top.rs", "needle up here\n");
        files.file("sub/inner.rs", "needle down here\n");
        let mut ed = in_project(&files, "top.rs");

        ex(&mut ed, "replace sub/ /needle/pin/");
        ed.apply(cmd(Action::Results(ResultsCmd::ApplyAll)));

        let line = |name: &str| {
            let path = files.0.join(name);
            let entry = ed.buffers.iter().find(|b| b.buffer.path.as_deref() == Some(&*path));
            entry.map(|b| b.buffer.line(0))
        };
        assert_eq!(line("top.rs").unwrap(), "needle up here", "outside the scope");
        assert_eq!(line("sub/inner.rs").unwrap(), "pin down here");
    }

    #[test]
    fn a_scope_needs_a_pattern() {
        let files = Files::new("scope-no-pattern");
        files.dir("sub");
        files.file("sub/inner.rs", "needle\n");
        let mut ed = in_project(&files, "sub/inner.rs");
        ed.session.tree_root = Some(files.0.clone());

        ex(&mut ed, "replace sub/ //pin/");

        assert_eq!(ed.session.status, "a scope needs a pattern — `:replace src/ /old/new/`");
    }

    #[test]
    fn the_alternate_swaps_an_opened_result_back_to_the_pane() {
        let files = project("results-alt");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");
        ed.apply(cmd(Action::Results(ResultsCmd::Move(1))));
        ed.apply(cmd(Action::Results(ResultsCmd::Open)));
        assert!(ed.window().results().is_none(), "the file displaced the pane");

        ex(&mut ed, "b#");

        let results = ed.window().results().expect("the pane is the alternate, like a tree");
        assert_eq!(results.selected(), 1, "as you left it");
    }

    #[test]
    fn results_brings_the_last_list_back_as_it_was() {
        let files = project("results-back");
        let mut ed = in_project(&files, "c.rs");
        ex(&mut ed, "find needle");
        ex(&mut ed, "replace //pin/");
        ed.apply(cmd(Action::Results(ResultsCmd::Move(1))));
        ed.apply(cmd(Action::Results(ResultsCmd::Apply)));

        // Leave through Enter, then wander off — the alternate slot moves on.
        ed.apply(cmd(Action::Results(ResultsCmd::Open)));
        let other = files.0.join("c.rs").to_string_lossy().into_owned();
        ex(&mut ed, &format!("e {other}"));

        ex(&mut ed, "results");

        let results = ed.window().results().expect("the list is back");
        assert!(results.is_applied(0), "decisions intact");
        assert!(results.replace.is_some(), "still armed");
    }

    #[test]
    fn results_with_nothing_to_bring_back_says_so() {
        let mut ed = editor("x\n");
        ex(&mut ed, "results");
        assert_eq!(ed.session.status, "no results to bring back — `:find` something first");
    }

    #[test]
    fn find_says_what_it_wants_when_given_nothing() {
        let mut ed = editor("x\n");
        ex(&mut ed, "find");
        assert_eq!(ed.session.status, "find what?");
        ex(&mut ed, "find~");
        assert_eq!(ed.session.status, "find what pattern?");
    }

    // ---- :resize ------------------------------------------------------------

    /// Lays the tree out in a fixed area and reports the focused pane's rect.
    fn pane(ed: &mut Editor) -> Rect {
        let focus = ed.focus();
        ed.layout(Rect::new(0, 0, 80, 24), TEST_CHROME)
            .into_iter()
            .find(|&(id, _)| id == focus)
            .map(|(_, r)| r)
            .unwrap()
    }

    /// Two panes side by side, focused on one of them.
    fn side_by_side() -> Editor {
        let mut ed = editor("text\n");
        split(&mut ed, Dir::Vertical);
        ed
    }

    #[test]
    fn resize_by_a_signed_amount_moves_the_divider() {
        let mut ed = side_by_side();
        let before = pane(&mut ed).width;

        ex(&mut ed, "resize +6");
        assert_eq!(pane(&mut ed).width, before + 6);

        ex(&mut ed, "resize -6");
        assert_eq!(pane(&mut ed).width, before, "and back");
    }

    #[test]
    fn resize_to_a_number_of_cells_lands_on_it() {
        let mut ed = side_by_side();
        let focus = ed.focus();
        // The pane has to have been told its size, since an absolute resize is
        // a delta from what it is now.
        let now = pane(&mut ed);
        ed.size_window(focus, now.width as usize, now.height as usize);

        ex(&mut ed, "resize 30");

        assert_eq!(pane(&mut ed).width, 30);
    }

    #[test]
    fn a_ratio_divides_the_split_the_window_is_in() {
        let mut ed = side_by_side();

        ex(&mut ed, "resize 1:2");

        // Both panes, left to right, so the ratio is read off the pair rather
        // than guessed from one. A split leaves the focus on the *new* window,
        // which is the second child — so the focused pane is the `2`.
        let mut widths: Vec<u16> = ed
            .layout(Rect::new(0, 0, 80, 24), TEST_CHROME)
            .into_iter()
            .map(|(_, r)| (r.x, r.width))
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_values()
            .collect();
        widths.sort_by_key(|w| *w);

        assert_eq!(widths.len(), 2);
        assert_eq!(widths[1] / widths[0], 2, "{widths:?} is not two to one");
        assert_eq!(pane(&mut ed).width, widths[1], "and the focused pane has the larger share");
        assert_eq!(ed.session.status, "", "nothing to report");
    }

    #[test]
    fn a_ratio_is_normalised_so_it_need_not_be_in_lowest_terms() {
        let mut ed = side_by_side();
        ex(&mut ed, "resize 1:2");
        let thirds = pane(&mut ed).width;

        ex(&mut ed, "resize 20:40");

        assert_eq!(pane(&mut ed).width, thirds, "20:40 is 1:2");
    }

    #[test]
    fn a_ratio_with_the_wrong_number_of_shares_says_how_many_it_wanted() {
        let mut ed = side_by_side();

        ex(&mut ed, "resize 1:2:1");

        assert_eq!(ed.session.status, "2 panes across that split, so 2 shares");
    }

    #[test]
    fn a_ratio_with_no_split_to_divide_says_so() {
        let mut ed = editor("text\n");

        ex(&mut ed, "resize 1:2");

        assert_eq!(ed.session.status, "nothing to divide the width with");
    }

    #[test]
    fn resizing_the_axis_a_window_does_not_split_on_says_so() {
        // Two panes side by side have no horizontal divider between them, so
        // there is no height to change.
        let mut ed = side_by_side();

        ex(&mut ed, "resize +3y");

        assert_eq!(ed.session.status, "no room to change the height");
    }

    #[test]
    fn both_axes_are_attempted_and_the_one_that_worked_still_works() {
        let mut ed = side_by_side();
        let before = pane(&mut ed).width;

        ex(&mut ed, "resize +6,+3");

        assert_eq!(pane(&mut ed).width, before + 6, "the width moved");
        assert_eq!(
            ed.session.status, "no room to change the height",
            "and only the half that could not is reported"
        );
    }

    #[test]
    fn a_resize_that_makes_no_sense_says_so_before_moving_anything() {
        let mut ed = side_by_side();
        let before = pane(&mut ed).width;

        ex(&mut ed, "resize wide");

        assert_eq!(ed.session.status, "`wide` is not a number of cells");
        assert_eq!(pane(&mut ed).width, before);
    }

    // ---- :symbols -----------------------------------------------------------

    /// The rows `:symbols` is offering.
    fn symbol_rows(ed: &Editor) -> Vec<String> {
        ed.session
            .picker
            .as_ref()
            .expect("the picker is up")
            .items()
            .iter()
            .map(|i| i.text.clone())
            .collect()
    }

    #[test]
    fn symbols_lists_what_you_would_navigate_to() {
        let d = ScratchDir::new("symbols-rust").written(
            "a.rs",
            "mod inner {\n    pub struct S { a: u32 }\n}\nfn main() {\n    let x = 1;\n}\n",
        );
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();

        ex(&mut ed, "symbols");

        assert_eq!(
            symbol_rows(&ed),
            ["inner  mod_item  1", "S  struct_item  2", "main  function_item  4"],
            "the module, the struct and the function — and not the field or the let"
        );
    }

    #[test]
    fn symbols_reaches_a_language_that_has_no_name_field() {
        // C hides the function name under a `declarator`, and C3 under a
        // `func_header`. Both are why the walk falls back to the first
        // identifier rather than trusting the field.
        let d =
            ScratchDir::new("symbols-c").written("a.c", "int add(int a, int b) { return a; }\n");
        let mut ed = Editor::open(format!("{}/a.c", d.path())).unwrap();

        ex(&mut ed, "symbols");

        assert_eq!(symbol_rows(&ed), ["add  function_definition  1"]);
    }

    #[test]
    fn choosing_a_symbol_lands_on_its_name() {
        let text = "fn alpha() {}\nfn beta() {}\n";
        let d = ScratchDir::new("symbols-jump").written("a.rs", text);
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();

        ex(&mut ed, "symbols");
        ed.apply(cmd(Action::PickNext));
        ed.apply(cmd(Action::PickAccept));

        assert_eq!(
            ed.cursor().unwrap().at,
            text.find("beta").unwrap(),
            "on the name, not on column zero of the line it is on"
        );
        assert_eq!(ed.session.mode, Mode::Normal, "and the overlay is gone");
    }

    #[test]
    fn symbols_says_so_when_there_is_nothing_to_show() {
        let d = ScratchDir::new("symbols-empty").written("a.rs", "let x = 1;\n");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();
        ex(&mut ed, "symbols");
        assert_eq!(ed.session.status, "no symbols in this file");
        assert!(ed.session.picker.is_none(), "an empty overlay is a worse answer");
    }

    #[test]
    fn symbols_says_so_when_bi_cannot_read_the_language() {
        let d = ScratchDir::new("symbols-plain").written("notes.txt", "just words\n");
        let mut ed = Editor::open(format!("{}/notes.txt", d.path())).unwrap();

        ex(&mut ed, "symbols");

        assert_eq!(ed.session.status, "no grammar for this file");
    }

    #[test]
    fn set_syntax_gives_symbols_to_a_file_that_had_none() {
        // The two features meeting: name the language, and the list appears.
        let d = ScratchDir::new("symbols-set-syntax").written("script", "def go():\n    pass\n");
        let mut ed = Editor::open(format!("{}/script", d.path())).unwrap();
        ex(&mut ed, "symbols");
        assert_eq!(ed.session.status, "no grammar for this file");

        ex(&mut ed, "set syntax python");
        ex(&mut ed, "symbols");

        assert_eq!(symbol_rows(&ed), ["go  function_definition  1"]);
    }

    // ---- :set syntax --------------------------------------------------------

    /// The grammar a buffer is actually being read with.
    fn parsed_as(ed: &Editor) -> Option<&'static str> {
        ed.buffers
            .iter()
            .find(|b| Some(b.id) == ed.window().buffer())
            .unwrap()
            .syntax
            .as_ref()
            .map(|s| s.filetype())
    }

    #[test]
    fn set_syntax_reads_a_file_as_something_its_name_does_not_say() {
        let d = ScratchDir::new("set-syntax").written("script", "echo $HOME\n");
        let mut ed = Editor::open(format!("{}/script", d.path())).unwrap();
        assert_eq!(parsed_as(&ed), None, "no extension, so nothing to go on");

        ex(&mut ed, "set syntax bash");

        assert_eq!(parsed_as(&ed), Some("bash"), "and now it is a shell script");
    }

    #[test]
    fn set_syntax_auto_hands_the_decision_back() {
        let d = ScratchDir::new("set-syntax-auto").written("a.rs", "fn main() {}\n");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();

        ex(&mut ed, "set syntax python");
        assert_eq!(parsed_as(&ed), Some("python"), "an extension can be overruled");

        ex(&mut ed, "set syntax auto");
        assert_eq!(parsed_as(&ed), Some("rust"), "and the name gets its say back");
    }

    #[test]
    fn set_syntax_refuses_a_language_bi_cannot_parse() {
        let mut ed = editor("x\n");

        ex(&mut ed, "set syntax cobol");

        assert_eq!(ed.session.status, "no grammar for cobol: cobol");
        assert_eq!(ed.session.options.syntax, "", "and nothing was set");
    }

    #[test]
    fn set_syntax_with_no_value_reports_what_is_in_force() {
        let d = ScratchDir::new("set-syntax-report").written("a.rs", "fn main() {}\n");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();

        ex(&mut ed, "set syntax");
        assert_eq!(ed.session.status, "syntax=rust", "the file's own, not the empty override");

        ex(&mut ed, "set syntax toml");
        ex(&mut ed, "set syntax");
        assert_eq!(ed.session.status, "syntax=toml");
    }

    #[test]
    fn setting_something_else_does_not_reparse_the_world() {
        // `resolve_options` runs on every `:set`, and it compares before it
        // rebuilds — this pins that the grammar survives an unrelated one.
        let d = ScratchDir::new("set-syntax-stable").written("a.rs", "fn main() {}\n");
        let mut ed = Editor::open(format!("{}/a.rs", d.path())).unwrap();

        ex(&mut ed, "set number 3");

        assert_eq!(parsed_as(&ed), Some("rust"));
    }

    // ---- a selection on the `:` line ----------------------------------------

    #[test]
    fn colon_with_a_selection_says_what_it_is_about() {
        let mut ed = visual("a\nb\nc\nd\n", 2, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::EnterCommandMode));

        assert_eq!(
            cmdline(&ed),
            ("'v ".to_string(), 3),
            "the scope, with the cursor past it so the command is what you type next"
        );
    }

    #[test]
    fn colon_with_no_selection_opens_empty() {
        let mut ed = editor("a\nb\n");
        ed.apply(cmd(Action::EnterCommandMode));

        assert_eq!(cmdline(&ed).0, "", "nothing selected, nothing to say about lines");
    }

    #[test]
    fn a_rectangle_is_still_a_rectangle_on_the_colon_line() {
        // The bug: `Mode::Command` replaces `Mode::Visual`, so the flag saying
        // "this is a block" was destroyed by the keystroke that opens the
        // command that was going to act on it.
        let mut ed = visual("abcd\nefgh\nijkl\n", 1, Shape::Block);
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        assert_eq!(ed.visual(), Some(Shape::Block));

        ed.apply(cmd(Action::EnterCommandMode));

        assert_eq!(ed.visual(), Some(Shape::Block), "and it survives the colon");
    }

    #[test]
    fn a_stale_shape_cannot_paint_a_later_normal_mode() {
        let mut ed = visual("abcd\nefgh\n", 1, Shape::Block);
        ed.apply(cmd(Action::EnterCommandMode));
        ed.apply(cmd(Action::CommandCancel));

        assert_eq!(ed.visual(), None, "the mode decides, not the leftover");
    }

    /// Columns 4..=8 of three rows — the names, and not the `let` or the
    /// `= 1;` on either side of them. `upper`, not `lower`: over this text the
    /// whole-line answer and the column answer differ, which is the only way
    /// the assertion says anything.
    fn block_over_the_names(text: &str) -> Editor {
        let mut ed = visual(text, 4, Shape::Block);
        for _ in 0..4 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }
        for _ in 0..2 {
            ed.apply(cmd(Action::Move(Motion::Down)));
        }
        ed
    }

    const NAMES: &str = "let alpha = 1;\nlet beta  = 2;\nlet gamma = 3;\n";

    #[test]
    fn case_over_a_rectangle_takes_the_columns_and_not_the_lines() {
        let mut ed = block_over_the_names(NAMES);

        ex(&mut ed, "'v case upper");

        assert_eq!(whole(&ed), "let ALPHA = 1;\nlet BETA  = 2;\nlet GAMMA = 3;\n");
    }

    /// The other half of the same rule: `'<,'>` is rows, in bi as in vim, and
    /// typing it over the prefill is how you ask for them.
    #[test]
    fn the_row_spelling_over_a_rectangle_takes_the_rows() {
        let mut ed = block_over_the_names(NAMES);

        ex(&mut ed, "'<,'>case upper");

        assert_eq!(whole(&ed), "LET ALPHA = 1;\nLET BETA  = 2;\nLET GAMMA = 3;\n");
    }

    /// What the `:` line prefills is what it does, without a keystroke of
    /// help — the whole point of putting the scope in text you can see.
    #[test]
    fn the_prefill_is_what_the_command_acts_on() {
        let mut ed = block_over_the_names(NAMES);
        ed.apply(cmd(Action::EnterCommandMode));
        for c in "case upper".chars() {
            ed.apply(cmd(Action::CommandChar(c)));
        }
        ed.apply(cmd(Action::CommandExecute));

        assert_eq!(whole(&ed), "let ALPHA = 1;\nlet BETA  = 2;\nlet GAMMA = 3;\n");
    }

    #[test]
    fn case_over_a_rectangle_is_one_undo_step() {
        let text = "AB\nCD\nEF\n";
        let mut ed = visual(text, 0, Shape::Block);
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::Move(Motion::Down)));

        ex(&mut ed, "'v case lower");
        assert_eq!(whole(&ed), "aB\ncD\neF\n");

        ed.apply(cmd(Action::Undo));
        assert_eq!(whole(&ed), text, "three rows back in one press");
    }

    /// A charwise selection is its characters, not the rows they sit on —
    /// which is the thing `'<,'>` could not say and the reason `'v` exists.
    #[test]
    fn case_over_a_charwise_selection_takes_what_is_highlighted() {
        let mut ed = visual("let someName = 1;\n", 4, Shape::Chars);
        for _ in 0..7 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }

        ex(&mut ed, "'v case snake");

        assert_eq!(whole(&ed), "let some_name = 1;\n", "and not the `let` in front of it");
    }

    /// A failed command keeps the selection *actionable*, not just painted:
    /// visual mode comes back, so the next `:` prefills `'v ` again and the
    /// fixed command acts on what you were looking at.
    /// See `docs/specs/cmdline.md`.
    #[test]
    fn a_failed_command_returns_to_visual_mode() {
        let mut ed = visual("hello world\n", 0, Shape::Chars);
        for _ in 0..4 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }

        ex(&mut ed, "'v case invalid");

        assert!(ed.session.status.starts_with("case what?"), "{}", ed.session.status);
        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars), "the selection is still live");
        assert_eq!(ed.selections().unwrap().primary().range(), (0, 4));

        // And the retry is the whole point: `:` prefills the scope again.
        ed.apply(cmd(Action::EnterCommandMode));
        assert_eq!(ed.session.mode, Mode::Command(CmdLine::from("'v ")));
        for c in "case upper".chars() {
            ed.apply(cmd(Action::CommandChar(c)));
        }
        ed.apply(cmd(Action::CommandExecute));
        assert_eq!(whole(&ed), "HELLO world\n", "the fixed command acts on the selection");
        assert_eq!(ed.session.mode, Mode::Normal, "consumed, so it collapses");
    }

    #[test]
    fn an_unknown_command_returns_to_visual_mode_too() {
        let mut ed = visual("hello\n", 0, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));

        ex(&mut ed, "flub");

        assert_eq!(ed.session.status, "not a command: flub");
        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars));
    }

    /// The shape survives the round trip: a rectangle interrupted is a
    /// rectangle restored.
    #[test]
    fn the_shape_survives_a_failed_command() {
        let mut ed = block_over_the_names(NAMES);

        ex(&mut ed, "'v case sideways");

        assert_eq!(ed.session.mode, Mode::Visual(Shape::Block));
    }

    #[test]
    fn esc_on_the_line_puts_the_selection_back() {
        let mut ed = visual("hello\n", 0, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::EnterCommandMode));

        ed.apply(cmd(Action::CommandCancel));

        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars));
    }

    #[test]
    fn backspacing_off_the_line_puts_the_selection_back() {
        let mut ed = visual("hello\n", 0, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::EnterCommandMode));

        // The prefill is `'v ` — three characters, then the leaving press.
        for _ in 0..4 {
            ed.apply(cmd(Action::CommandBackspace));
        }

        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars));
    }

    /// `:m` keeps the moved block selected so you can move it again — and now
    /// the mode agrees, so `:` prefills `'v ` for the next nudge.
    #[test]
    fn a_command_that_keeps_the_selection_returns_to_visual_mode() {
        let mut ed = visual("one\ntwo\nthree\n", 0, Shape::Lines);
        ed.apply(cmd(Action::Move(Motion::Down)));

        ex(&mut ed, "'v m $");

        assert_eq!(whole(&ed), "three\none\ntwo\n");
        assert_eq!(ed.session.mode, Mode::Visual(Shape::Lines), "still selected, and it says so");
    }

    /// A command with no use for the scope leaves the selection exactly as
    /// interesting as it found it.
    #[test]
    fn a_command_that_ignores_the_selection_returns_to_visual_mode() {
        let mut ed = visual("hello\n", 0, Shape::Chars);
        ed.apply(cmd(Action::Move(Motion::Right)));

        ex(&mut ed, "noh");

        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars));
    }

    /// `:s` inside a rectangle stays inside it. The columns are the scope,
    /// and the same walk that respells them substitutes in them.
    #[test]
    fn substitute_over_a_rectangle_stays_in_the_columns() {
        let mut ed = block_over_the_names("let alpha = 1;\nlet beta  = 2;\nlet gamma = 3;\n");

        ex(&mut ed, "'v s/a/X/g");

        assert_eq!(
            whole(&ed),
            "let XlphX = 1;\nlet betX  = 2;\nlet gXmmX = 3;\n",
            "and never the `a` in the text on either side"
        );
    }

    /// A command that can only work in whole lines, handed a rectangle,
    /// widens to the rows and says so rather than doing it quietly.
    #[test]
    fn a_command_that_needs_rows_widens_and_says_so() {
        let mut ed = block_over_the_names("let alpha = 1;\nlet beta  = 2;\nlet gamma = 3;\n");

        ex(&mut ed, "'v m 0");

        assert_eq!(ed.session.status, "whole lines");
        assert_eq!(
            whole(&ed),
            "let alpha = 1;\nlet beta  = 2;\nlet gamma = 3;\n",
            "already at the top, so the rows are the same three"
        );
    }

    /// Every cursor's own selection, not the span between the first and the
    /// last — which is what a line range would have made of them.
    #[test]
    fn the_selection_scope_reaches_every_cursor_and_nothing_between_them() {
        let mut ed = editor("foo_bar\nzzz\nfoo_bar\n");
        ed.set_cursor(Cursor::at(0));
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        ed.apply(cmd(Action::AddCursorLine { below: true }));
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
        for _ in 0..6 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }

        ex(&mut ed, "'v case camel");

        assert_eq!(whole(&ed), "fooBar\nzzz\nfooBar\n", "the middle row is not selected");
    }

    #[test]
    fn case_over_a_range_takes_whole_lines() {
        let mut ed = editor("ONE\nTWO\nTHREE\n");

        ex(&mut ed, "2,3case lower");

        assert_eq!(whole(&ed), "ONE\ntwo\nthree\n");
        assert_eq!(ed.session.status, "2 lines recased");
    }

    #[test]
    fn case_with_no_range_is_still_the_word_under_the_cursor() {
        let mut ed = editor("let someName = 1;\n");
        ed.set_cursor(Cursor::at(4));

        ex(&mut ed, "case snake");

        assert_eq!(whole(&ed), "let some_name = 1;\n", "unchanged by any of this");
    }

    #[test]
    fn a_write_still_refuses_the_range_the_colon_line_offered_it() {
        // The prefill is not a licence: `:w` cannot write part of a file, and
        // the selection arriving for free must not turn that into a partial
        // write done quietly.
        let mut ed = visual("a\nb\nc\n", 0, Shape::Lines);
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::EnterCommandMode));

        ex(&mut ed, "'<,'>w out.txt");

        assert_eq!(ed.session.status, "`:w` takes no range");
    }

    // ---- retab --------------------------------------------------------------

    /// The buffer as one string, for the tests that care what was written
    /// rather than where the cursor ended up.
    fn text(ed: &Editor) -> String {
        ed.buffer().unwrap().rope().to_string()
    }

    #[test]
    fn retab_converts_tabs_to_what_the_options_ask_for() {
        let mut ed = editor("fn main()\n{\n\tlet x = 1;\n\t\tdeep();\n}\n");

        ex(&mut ed, "retab");

        assert_eq!(text(&ed), "fn main()\n{\n    let x = 1;\n        deep();\n}\n");
        assert_eq!(ed.session.status, "2 lines retabbed to spaces");
    }

    #[test]
    fn retab_converts_the_other_way_too() {
        let mut ed = editor("{\n    a;\n        b;\n}\n");
        ex(&mut ed, "set expandtab false");

        ex(&mut ed, "retab");

        assert_eq!(text(&ed), "{\n\ta;\n\t\tb;\n}\n");
        assert_eq!(ed.session.status, "2 lines retabbed to tabs");
    }

    #[test]
    fn retab_keeps_the_file_the_width_it_was() {
        // A tab was eight columns, so it becomes eight spaces — not four. The
        // characters change; the layout does not, which is the only promise
        // worth making about a conversion.
        let mut ed = editor("\tx\n");
        ex(&mut ed, "set tab_width 8");

        ex(&mut ed, "retab");

        assert_eq!(text(&ed), "        x\n");
    }

    #[test]
    fn retab_does_not_reach_inside_the_line() {
        let mut ed = editor("\tmsg = \"a\tb\";\t// note\n");

        ex(&mut ed, "retab");

        assert_eq!(
            text(&ed),
            "    msg = \"a\tb\";\t// note\n",
            "the tab in the string and the aligning one are content, not indentation"
        );
    }

    #[test]
    fn retab_leaves_a_blank_line_to_the_trimmer() {
        let mut ed = editor("\ta;\n\t\t\n\tb;\n");

        ex(&mut ed, "retab");

        assert_eq!(text(&ed), "    a;\n\t\t\n    b;\n", "a line with no text has no indent");
        assert_eq!(ed.session.status, "2 lines retabbed to spaces");
    }

    #[test]
    fn retab_takes_a_range_and_a_bare_one_takes_the_file() {
        let mut ed = editor("\ta;\n\tb;\n\tc;\n");

        ex(&mut ed, "2,3retab");

        assert_eq!(text(&ed), "\ta;\n    b;\n    c;\n", "only the lines named");
    }

    #[test]
    fn retab_refuses_a_line_that_is_not_there() {
        let mut ed = editor("\ta;\n");

        ex(&mut ed, "1,99retab");

        assert_eq!(ed.session.status, "no line 99");
        assert_eq!(text(&ed), "\ta;\n", "and changed nothing");
    }

    #[test]
    fn retab_says_when_there_was_nothing_to_do() {
        let mut ed = editor("    a;\n");

        ex(&mut ed, "retab");

        assert_eq!(ed.session.status, "indentation is already what the options ask for");
    }

    #[test]
    fn retab_is_one_undo_step() {
        let mut ed = editor("\ta;\n\tb;\n\tc;\n");

        ex(&mut ed, "retab");
        ed.apply(cmd(Action::Undo));

        assert_eq!(text(&ed), "\ta;\n\tb;\n\tc;\n", "three lines back in one press");
    }

    #[test]
    fn retab_carries_the_cursor_with_its_line() {
        let mut ed = editor("\ta;\n\tb;\n\tlanding;\n");
        // On the `l` of `landing`, which is one tab in on the third row.
        ed.set_cursor(Cursor::at(text(&ed).find("landing").unwrap()));

        ex(&mut ed, "retab");

        let at = ed.cursor().unwrap().at;
        assert_eq!(
            text(&ed)[..at].chars().count(),
            text(&ed).find("landing").unwrap(),
            "still on the same character, not clamped to the start of the line"
        );
    }

    #[test]
    fn retab_takes_no_argument() {
        let mut ed = editor("\ta;\n");

        ex(&mut ed, "retab 8");

        assert_eq!(
            ed.session.status,
            "retab takes no argument — it follows tab_width and expandtab"
        );
        assert_eq!(text(&ed), "\ta;\n");
    }

    #[test]
    fn sort_orders_the_whole_file_when_nothing_narrows_it() {
        let mut ed = editor("banana\napple\ncherry\n");
        ex(&mut ed, "sort");
        assert_eq!(whole(&ed), "apple\nbanana\ncherry\n");
        assert_eq!(ed.session.status, "3 lines sorted");
    }

    #[test]
    fn sort_takes_a_range_and_touches_nothing_outside_it() {
        let mut ed = editor("zeta\nbeta\nalpha\ngamma\n");
        ex(&mut ed, "2,3sort");
        assert_eq!(whole(&ed), "zeta\nalpha\nbeta\ngamma\n");
    }

    #[test]
    fn sort_over_a_selection_takes_the_rows_it_touches() {
        let mut ed = visual("delta\ncharlie\nbravo\nalpha\n", 0, Shape::Lines);
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::Move(Motion::Down)));

        ex(&mut ed, "'v sort");

        assert_eq!(whole(&ed), "bravo\ncharlie\ndelta\nalpha\n");
        assert_eq!(ed.session.mode, Mode::Normal, "the selection was consumed");
    }

    #[test]
    fn sort_bang_descends() {
        let mut ed = editor("banana\napple\ncherry\n");
        ex(&mut ed, "sort!");
        assert_eq!(whole(&ed), "cherry\nbanana\napple\n");
    }

    #[test]
    fn sort_n_compares_the_numbers_rather_than_the_digits() {
        let mut ed = editor("item 12\nitem 9\nitem 100\n");
        ex(&mut ed, "sort n");
        assert_eq!(whole(&ed), "item 9\nitem 12\nitem 100\n");
    }

    #[test]
    fn sort_u_drops_the_duplicate_and_counts_it() {
        let mut ed = editor("b\na\nb\n");
        ex(&mut ed, "sort u");
        assert_eq!(whole(&ed), "a\nb\n");
        assert_eq!(ed.session.status, "3 lines sorted, 1 duplicate dropped");
    }

    #[test]
    fn sort_is_one_undo_step_and_lands_on_the_first_line() {
        let mut ed = editor("banana\napple\ncherry\n");
        ed.set_cursor(Cursor::at(10));

        ex(&mut ed, "sort");
        assert_eq!(ed.selections().unwrap().cursor().at, 0, "the block starts here");

        ed.apply(cmd(Action::Undo));
        assert_eq!(whole(&ed), "banana\napple\ncherry\n", "one press, all of it back");
    }

    #[test]
    fn sort_on_an_ordered_range_says_so_and_adds_no_history() {
        let mut ed = editor("a\nb\nc\n");
        ex(&mut ed, "sort");
        assert_eq!(ed.session.status, "already sorted");

        ed.apply(cmd(Action::Undo));
        assert_eq!(whole(&ed), "", "`u` reaches the typing — sort left no revision on top of it");
    }

    #[test]
    fn sort_last_line_without_terminator_stays_terminatorless() {
        let mut ed = editor("b\na");
        ex(&mut ed, "sort");
        assert_eq!(whole(&ed), "a\nb");
    }

    #[test]
    fn sort_names_the_flag_it_does_not_have() {
        let mut ed = editor("b\na\n");
        ex(&mut ed, "sort x");
        assert_eq!(ed.session.status, "`x` is not a sort flag — n, i, u");
        assert_eq!(whole(&ed), "b\na\n", "and nothing changed");
    }

    #[test]
    fn sort_refuses_a_line_that_is_not_there() {
        let mut ed = editor("b\na\n");
        ex(&mut ed, "2,99sort");
        assert_eq!(ed.session.status, "no line 99");
        assert_eq!(whole(&ed), "b\na\n");
    }

    #[test]
    fn retab_follows_the_project_the_way_everything_else_does() {
        // The whole point, in one test: a tab-indented file in a project whose
        // .editorconfig asks for four spaces.
        let files = Files::new("retab-editorconfig");
        files.file(".editorconfig", "root = true\n[*.c3]\nindent_style = space\nindent_size = 4\n");
        let path = files.file("main.c3", "fn main()\n{\n\tio::printn(\"hi\");\n}\n");
        let mut ed = Editor::open(&path).unwrap();

        ex(&mut ed, "retab");
        ex(&mut ed, "w");

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn main()\n{\n    io::printn(\"hi\");\n}\n"
        );
    }

    #[test]
    fn a_later_set_outranks_the_project_here_as_everywhere() {
        let files = Files::new("retab-set-wins");
        files.file(".editorconfig", "root = true\n[*.c3]\nindent_style = space\nindent_size = 4\n");
        let path = files.file("main.c3", "{\n    a;\n}\n");
        let mut ed = Editor::open(&path).unwrap();

        ex(&mut ed, "set expandtab false");
        ex(&mut ed, "retab");

        assert_eq!(
            text(&ed),
            "{\n\ta;\n}\n",
            "`:set` is the layer above the project, and :retab reads the result"
        );
    }

    // ---- decorations --------------------------------------------------------

    use crate::decoration::{Decoration, Layer};
    use crate::theme::Style;

    /// The guides on each row, as columns, for a whole-file query.
    fn guides(ed: &Editor) -> Vec<(usize, Vec<usize>)> {
        let rows = ed.buffer().unwrap().line_count();
        let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
        for decoration in ed.decorations(ed.focus(), 0..rows) {
            let Decoration::Overlay { row, col, layer, .. } = decoration else {
                panic!("a guide is an overlay");
            };
            assert_eq!(layer, Layer::Under, "or a selected line stops looking selected");
            match out.iter_mut().find(|(at, _)| *at == row) {
                Some((_, cols)) => cols.push(col),
                None => out.push((row, vec![col])),
            }
        }
        out
    }

    #[test]
    fn a_guide_goes_down_every_level_of_indentation() {
        let ed = editor("fn main() {\n    let x = 1;\n        deep();\n}\n");

        assert_eq!(guides(&ed), [(1, vec![0]), (2, vec![0, 4])]);
    }

    #[test]
    fn a_blank_line_takes_the_smaller_of_its_neighbours() {
        // Inside a block the guides carry on; where a block ends they stop,
        // which is what `min` says and why it is not `max`.
        let ed = editor("    a\n\n    b\n\nc\n");

        assert_eq!(
            guides(&ed),
            [(0, vec![0]), (1, vec![0]), (2, vec![0])],
            "row 3 sits between an indented line and an unindented one, so it \
             belongs to no block and shows nothing"
        );
    }

    #[test]
    fn guides_count_columns_so_a_tab_is_as_wide_as_it_is_drawn() {
        let mut ed = editor("\t\tdeep\n");
        ex(&mut ed, "set expandtab false");
        ex(&mut ed, "set tab_width 8");

        assert_eq!(guides(&ed), [(0, vec![0, 8])], "two tabs at eight columns each");
    }

    #[test]
    fn guides_are_bounded_by_the_rows_that_were_asked_for() {
        let ed = editor("    a\n    b\n    c\n");

        let visible = ed.decorations(ed.focus(), 1..2);

        assert_eq!(visible.len(), 1, "one row asked for, one row's worth back");
        assert!(matches!(visible[0], Decoration::Overlay { row: 1, col: 0, .. }));
    }

    #[test]
    fn a_provider_that_is_off_produces_nothing() {
        let mut ed = editor("    a\n");
        ex(&mut ed, "set indent_guides false");

        assert!(ed.decorations(ed.focus(), 0..1).is_empty());
    }

    #[test]
    fn a_marker_is_painted_where_it_stands() {
        let ed = editor("// TODO: rewrite\nlet x = 1;\n// FIX: this\n");

        let found: Vec<(std::ops::Range<usize>, Style)> = ed
            .decorations(ed.focus(), 0..3)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Repaint { range, style, layer } => {
                    assert_eq!(layer, Layer::Under);
                    Some((range, style))
                }
                _ => None,
            })
            .collect();

        let ui = &ed.theme().ui;
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, 3..8, "`TODO:` and not the space after it");
        assert_eq!(found[0].1, ui.todo_todo);
        assert_eq!(found[1].1, ui.todo_fix, "and the second line's marker is its own colour");
    }

    #[test]
    fn markers_off_produce_nothing() {
        let mut ed = editor("// TODO: rewrite\n");
        ex(&mut ed, "set todo_comments false");
        ex(&mut ed, "set indent_guides false");

        assert!(ed.decorations(ed.focus(), 0..1).is_empty());
    }

    #[test]
    fn a_colour_literal_is_drawn_in_the_colour_it_names() {
        let mut ed = editor("bg = \"#fb4934\"\n");
        ex(&mut ed, "set todo_comments false");
        ex(&mut ed, "set indent_guides false");

        let found = ed.decorations(ed.focus(), 0..1);

        assert_eq!(found.len(), 1);
        let Decoration::Repaint { range, style, layer } = &found[0] else { panic!("a repaint") };
        assert_eq!(*range, 6..13, "the literal, not the quotes around it");
        assert_eq!(style.bg, Some(crate::theme::Color::Rgb(0xfb, 0x49, 0x34)));
        assert_eq!(style.fg, Some(crate::theme::Color::Rgb(0, 0, 0)), "readable on that red");
        assert_eq!(*layer, Layer::Under);
    }

    #[test]
    fn swatches_off_produce_nothing() {
        let mut ed = editor("#fb4934\n");
        ex(&mut ed, "set color_swatches false");
        ex(&mut ed, "set indent_guides false");
        ex(&mut ed, "set todo_comments false");

        assert!(ed.decorations(ed.focus(), 0..1).is_empty());
    }

    // ---- whitespace ---------------------------------------------------------

    /// An editor over `text` with `:whitespace` on and the guides out of the
    /// way, so what comes back is this provider's and only this provider's.
    fn shown(text: &str) -> Editor {
        let mut ed = editor(text);
        ex(&mut ed, "set indent_guides false");
        ex(&mut ed, "whitespace");
        ed
    }

    /// The marks on each row, as (column, glyph). The pilcrow has no column of
    /// its own — it is drawn past the end of the line — and comes back as
    /// `None`.
    fn marks(ed: &Editor) -> Vec<(usize, Option<usize>, String)> {
        let rows = ed.buffer().unwrap().line_count();
        let style = ed.theme().ui.whitespace;
        ed.decorations(ed.focus(), 0..rows)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Overlay { row, col, text, style: s, layer } if s == style => {
                    assert_eq!(layer, Layer::Under, "or a selected line stops looking selected");
                    Some((row, Some(col), text))
                }
                Decoration::Eol { row, text, style: s } if s == style => Some((row, None, text)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_space_is_marked_and_nothing_else_is() {
        let ed = shown("a b  c\n");

        assert_eq!(
            marks(&ed),
            [
                (0, Some(1), WS_SPACE.into()),
                (0, Some(3), WS_SPACE.into()),
                (0, Some(4), WS_SPACE.into()),
                (0, None, WS_EOL.into()),
            ],
            "the blanks, in the columns they occupy, and the letters left alone"
        );
    }

    #[test]
    fn a_tab_is_marked_at_the_column_it_starts_in() {
        let mut ed = editor("\tx\ty\n");
        ex(&mut ed, "set indent_guides false");
        ex(&mut ed, "set expandtab false");
        ex(&mut ed, "set tab_width 8");
        ex(&mut ed, "whitespace");

        assert_eq!(
            marks(&ed),
            [
                (0, Some(0), WS_TAB.into()),
                // The second tab starts at column 9 and runs to the next stop
                // at 16 — an arrow at 16 would be pointing from the wrong end.
                (0, Some(9), WS_TAB.into()),
                (0, None, WS_EOL.into()),
            ],
        );
    }

    #[test]
    fn a_tab_leaves_the_rest_of_its_expansion_alone() {
        let mut ed = editor("\tab\n");
        ex(&mut ed, "set indent_guides false");
        ex(&mut ed, "whitespace");

        let cols: Vec<_> = marks(&ed).into_iter().filter_map(|(_, col, _)| col).collect();
        assert_eq!(cols, [0], "one mark for one tab, not four for the columns it covers");
    }

    #[test]
    fn a_non_breaking_space_does_not_look_like_a_space() {
        let ed = shown("a\u{a0}b\n");

        assert_eq!(marks(&ed)[0], (0, Some(1), WS_NBSP.into()), "which is the whole point of it");
    }

    #[test]
    fn the_last_line_says_whether_it_ends_in_a_newline() {
        assert_eq!(
            marks(&shown("a\nb\n")).last().unwrap(),
            &(1, None, WS_EOL.into()),
            "a file that ends in one gets a pilcrow on its last row"
        );
        assert_eq!(
            marks(&shown("a\nb")),
            [(0, None, WS_EOL.into())],
            "and one that does not gets none there — the absence is the report"
        );
    }

    #[test]
    fn a_blank_line_is_a_pilcrow_and_nothing_else() {
        assert_eq!(marks(&shown("a\n\nb\n"))[1], (1, None, WS_EOL.into()));
    }

    #[test]
    fn the_marks_are_bounded_by_the_rows_that_were_asked_for() {
        let ed = shown("a b\nc d\ne f\n");

        let visible = ed.decorations(ed.focus(), 1..2);

        assert!(
            visible.iter().all(|d| matches!(
                d,
                Decoration::Overlay { row: 1, .. } | Decoration::Eol { row: 1, .. }
            )),
            "one row asked for, one row's worth back"
        );
    }

    #[test]
    fn whitespace_off_produces_nothing() {
        let mut ed = editor("a b\n");
        ex(&mut ed, "set indent_guides false");

        assert!(ed.decorations(ed.focus(), 0..1).is_empty());
    }

    #[test]
    fn the_guides_stand_down_while_the_blanks_are_on_show() {
        let mut ed = editor("    x\n");
        ex(&mut ed, "whitespace");

        let at_zero: Vec<String> = ed
            .decorations(ed.focus(), 0..1)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Overlay { col: 0, text, .. } => Some(text),
                _ => None,
            })
            .collect();

        assert_eq!(at_zero, [WS_SPACE], "the bullet, and no guide under it");
    }

    #[test]
    fn a_blank_line_shows_nothing_that_is_not_there() {
        // The bug this rule exists for: a guide at column 0 of an empty line
        // has no character under it to be overwritten by, so it survived and
        // read as a space the file does not contain.
        let mut ed = editor("    a\n\n    b\n");
        ex(&mut ed, "whitespace");

        assert_eq!(
            marks(&ed).iter().filter(|(row, ..)| *row == 1).collect::<Vec<_>>(),
            [&(1, None, WS_EOL.to_string())],
            "one pilcrow, and not one mark before it"
        );
        assert!(
            !ed.decorations(ed.focus(), 1..2).iter().any(|d| matches!(
                d,
                Decoration::Overlay { text, .. } if text == GUIDE
            )),
            "and no guide either"
        );
    }

    #[test]
    fn the_guides_come_back_when_the_blanks_go_away() {
        let mut ed = editor("    x\n");
        ex(&mut ed, "whitespace on");
        ex(&mut ed, "whitespace off");

        let at_zero: Vec<String> = ed
            .decorations(ed.focus(), 0..1)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Overlay { col: 0, text, .. } => Some(text),
                _ => None,
            })
            .collect();

        assert_eq!(at_zero, [GUIDE], "standing down is not the same as being turned off");
    }

    #[test]
    fn the_pilcrow_comes_before_the_context_that_follows_the_line() {
        let (_d, mut ed) = in_c_block("ws-context", "do_something");
        ex(&mut ed, "whitespace");

        let eols: Vec<String> = ed
            .decorations(ed.focus(), 0..6)
            .into_iter()
            .filter_map(|d| match d {
                // Row 3 is the `}` that closes the `if`, so it carries both.
                Decoration::Eol { row: 3, text, .. } => Some(text),
                _ => None,
            })
            .collect();

        assert_eq!(eols.len(), 2);
        assert_eq!(eols[0], WS_EOL, "the end of the line, then what is past it");
        assert!(eols[1].contains("if (value == 0)"));
    }

    #[test]
    fn the_command_toggles_and_says_which_way_it_went() {
        let mut ed = editor("a b\n");
        assert!(!ed.options().whitespace, "off out of the box");

        ex(&mut ed, "whitespace");
        assert!(ed.options().whitespace);
        assert_eq!(ed.session.status, "whitespace=true");

        ex(&mut ed, "ws");
        assert!(!ed.options().whitespace, "the bare form is a toggle");

        ex(&mut ed, "whitespace on");
        assert!(ed.options().whitespace);
        ex(&mut ed, "whitespace on");
        assert!(ed.options().whitespace, "and the explicit form is not");

        ex(&mut ed, "whitespace off");
        assert!(!ed.options().whitespace);
    }

    #[test]
    fn the_command_refuses_a_word_it_does_not_know() {
        let mut ed = editor("a b\n");

        ex(&mut ed, "whitespace maybe");

        assert!(!ed.options().whitespace);
        assert_eq!(ed.session.status, "whitespace takes on, off, or nothing to toggle");
    }

    #[test]
    fn the_toggle_is_a_layer_a_later_file_keeps() {
        // `:set` is remembered as an override rather than as a value, and this
        // goes through the same door — so opening another buffer does not
        // quietly resolve it back off.
        let d = ScratchDir::new("ws-layer").written("a.txt", "a b\n").written("b.txt", "c d\n");
        let mut ed = Editor::open(format!("{}/a.txt", d.path())).unwrap();

        ex(&mut ed, "whitespace");
        ex(&mut ed, &format!("e {}/b.txt", d.path()));

        assert!(ed.options().whitespace, "still on in the file opened after it");
    }

    // ---- tree-sitter context ------------------------------------------------

    const C_BLOCK: &str = "\
int main(void) {
    if (value == 0) {
        do_something();
    }
    return 0;
}
";

    /// A C file with the cursor on the line holding `mark`.
    fn in_c_block(name: &str, mark: &str) -> (ScratchDir, Editor) {
        let d = ScratchDir::new(name).written("a.c", C_BLOCK);
        let mut ed = Editor::open(format!("{}/a.c", d.path())).unwrap();
        ed.set_cursor(Cursor::at(C_BLOCK.find(mark).expect("the test marks a row")));
        (d, ed)
    }

    /// Every annotation drawn over `rows` of `window`, as (row, text).
    fn context(
        ed: &Editor,
        window: WindowId,
        rows: std::ops::Range<usize>,
    ) -> Vec<(usize, String)> {
        let style = ed.theme().ui.context;
        ed.decorations(window, rows)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Eol { row, text, style: s } if s == style => Some((row, text)),
                _ => None,
            })
            .collect()
    }

    /// The line that opened the block, after the line that closes it, behind
    /// that language's comment marker. See
    /// `docs/specs/tree-sitter-context.md`.
    #[test]
    fn the_block_the_cursor_is_in_names_itself_on_its_closing_row() {
        let (_d, ed) = in_c_block("context", "do_something");

        assert_eq!(
            context(&ed, ed.focus(), 0..6),
            [(3, " // if (value == 0) {".to_string())],
            "the innermost block, on the row that closes it"
        );
    }

    #[test]
    fn depth_walks_outwards_and_zero_turns_it_off() {
        let (_d, mut ed) = in_c_block("context-depth", "do_something");

        ex(&mut ed, "set context_depth 2");
        assert_eq!(
            context(&ed, ed.focus(), 0..6),
            [(3, " // if (value == 0) {".to_string()), (5, " // int main(void) {".to_string()),],
        );

        ex(&mut ed, "set context_depth 0");
        assert!(context(&ed, ed.focus(), 0..6).is_empty());
    }

    /// Bounded by the rows on screen, like every other decoration: a closing
    /// brace scrolled past the bottom is not a row to draw on.
    #[test]
    fn an_annotation_off_the_screen_is_not_produced() {
        let (_d, ed) = in_c_block("context-rows", "do_something");

        assert!(context(&ed, ed.focus(), 0..3).is_empty(), "the closing row is row 3");
        assert_eq!(context(&ed, ed.focus(), 3..4).len(), 1);
    }

    /// An unfocused pane's cursor is not where you are looking.
    #[test]
    fn only_the_focused_window_says_where_it_is() {
        let (_d, mut ed) = in_c_block("context-focus", "do_something");
        sized(&mut ed);
        let first = ed.focus();
        ed.apply(cmd(Action::Window(WindowCmd::Split { dir: Dir::Vertical, path: None })));
        let second = ed.focus();
        assert_ne!(first, second);

        assert_eq!(context(&ed, second, 0..6).len(), 1, "the focused one still does");
        assert!(context(&ed, first, 0..6).is_empty(), "the other one does not");
    }

    /// No line comment, nothing to write it behind. A borrowed `//` in a JSON
    /// file reads as a mistake in the file.
    #[test]
    fn a_language_with_no_line_comment_gets_no_annotation() {
        let json = "{\n  \"a\": {\n    \"b\": 1\n  }\n}\n";
        let d = ScratchDir::new("context-json").written("a.json", json);
        let mut ed = Editor::open(format!("{}/a.json", d.path())).unwrap();
        ed.set_cursor(Cursor::at(json.find("\"b\"").unwrap()));

        assert!(context(&ed, ed.focus(), 0..5).is_empty());
    }

    // ---- the context header -------------------------------------------------

    const C_LONG: &str = "\
int main(void) {
    if (value == 0) {
        a();
        b();
        c();
        d();
        e();
    }
    return 0;
}
";

    /// `C_LONG` open in a pane `height` rows tall, scrolled to the cursor on
    /// the line holding `mark`.
    fn scrolled(name: &str, mark: &str, height: usize) -> (ScratchDir, Editor) {
        let d = ScratchDir::new(name).written("a.c", C_LONG);
        let mut ed = Editor::open(format!("{}/a.c", d.path())).unwrap();
        sized(&mut ed);
        ed.set_cursor(Cursor::at(C_LONG.find(mark).expect("the test marks a row")));
        let focus = ed.focus();
        ed.size_window(focus, 40, height);
        (d, ed)
    }

    /// Every header drawn over `rows`, as (row, text), with the padding cut off
    /// so the assertions read.
    fn header(ed: &Editor, rows: std::ops::Range<usize>) -> Vec<(usize, String)> {
        let style = ed.theme().ui.context_header;
        ed.decorations(ed.focus(), rows)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Overlay { row, col, text, style: s, .. } if s == style => {
                    assert_eq!(col, 0, "a header starts at the left of the text area");
                    Some((row, text.trim_end().to_string()))
                }
                _ => None,
            })
            .collect()
    }

    /// Scrolled into the `if`, the top row says which function you are in.
    #[test]
    fn the_top_row_carries_the_outermost_line_that_scrolled_off() {
        let (_d, ed) = scrolled("header", "e();", 5);

        // Five rows and a scrolloff margin of two puts the top of the pane at
        // row 4, so rows 0 and 1 — the two openers — are off it.
        assert_eq!(header(&ed, 4..9), [(4, "int main(void) {".to_string())]);
    }

    /// And depth 2 reads top-down, exactly as the file does.
    #[test]
    fn depth_adds_rows_downwards_outermost_first() {
        let (_d, mut ed) = scrolled("header-depth", "e();", 5);

        ex(&mut ed, "set context_header_depth 2");
        assert_eq!(
            header(&ed, 4..9),
            [(4, "int main(void) {".to_string()), (5, "    if (value == 0) {".to_string()),],
            "indentation kept, so the nesting reads",
        );

        ex(&mut ed, "set context_header_depth 0");
        assert!(header(&ed, 4..9).is_empty(), "and zero is off");
    }

    /// A header repeating a line three rows below it says nothing, and costs a
    /// row of code to say it.
    #[test]
    fn nothing_when_the_opening_line_is_still_on_screen() {
        let (_d, ed) = scrolled("header-visible", "b();", 20);

        assert!(header(&ed, 0..10).is_empty());
    }

    /// Scrolling up with `k` puts the cursor on the top row, and a header
    /// there hides the line being edited.
    #[test]
    fn a_header_never_covers_the_row_the_cursor_is_on() {
        let (_d, mut ed) = scrolled("header-cursor", "e();", 5);
        assert_eq!(header(&ed, 4..9).len(), 1, "drawn while the cursor is below it");

        // The cursor onto the top row, leaving the scroll where it was.
        ed.set_cursor(Cursor::at(C_LONG.find("c();").unwrap()));

        assert!(header(&ed, 4..9).is_empty());
    }

    /// A bar that stops at the last character is not a bar.
    #[test]
    fn the_header_is_as_wide_as_the_text_area() {
        let (_d, ed) = scrolled("header-width", "e();", 5);
        let gutter = ed.options().gutter_width(ed.buffer().unwrap().line_count());

        let style = ed.theme().ui.context_header;
        let drawn = ed.decorations(ed.focus(), 4..9);
        let width = drawn.iter().find_map(|d| match d {
            Decoration::Overlay { text, style: s, .. } if *s == style => Some(text.chars().count()),
            _ => None,
        });
        assert_eq!(width, Some(40 - gutter), "the pane, less the gutter");
    }

    // ---- the yank flash -----------------------------------------------------

    /// The ranges lit up right now, whatever produced them.
    fn lit(ed: &Editor) -> Vec<std::ops::Range<usize>> {
        let rows = ed.buffer().unwrap().line_count();
        let flash = ed.theme().ui.flash;
        ed.decorations(ed.focus(), 0..rows)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Repaint { range, style, .. } if style == flash => Some(range),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_yank_lights_up_what_it_read() {
        let mut ed = editor("alpha beta\n");
        ed.apply(Command {
            count: 1,
            action: Action::Operate {
                op: Operator::Yank,
                target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                count: 1,
                sink: Sink::Ring,
            },
        });

        assert_eq!(lit(&ed), vec![0..6], "the word and the space `yw` took");
    }

    #[test]
    fn a_linewise_yank_lights_the_line_and_a_block_lights_each_row() {
        let mut ed = editor("alpha\nbeta\n");
        ed.apply(cmd(Action::Operate {
            op: Operator::Yank,
            target: Target::Motion(Motion::CurrentLine),
            count: 1,
            sink: Sink::Ring,
        }));
        assert_eq!(lit(&ed), vec![0..6], "the row, terminator and all");

        let mut ed = editor("alpha\nbeta\n");
        ed.apply(cmd(Action::EnterVisual(Shape::Block)));
        ed.apply(cmd(Action::Move(Motion::Down)));
        ed.apply(cmd(Action::Move(Motion::Right)));
        ed.apply(cmd(Action::OperateSelection { op: Operator::Yank, sink: Sink::Ring }));

        assert_eq!(lit(&ed).len(), 2, "a rectangle is one span per row, not a bounding box");
    }

    #[test]
    fn nothing_else_flashes_and_the_next_command_puts_it_out() {
        let mut ed = editor("alpha beta\n");
        ed.apply(cmd(Action::Operate {
            op: Operator::Delete,
            target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
            count: 1,
            sink: Sink::Ring,
        }));
        assert!(lit(&ed).is_empty(), "a delete is visible in the text already");

        ed.apply(cmd(Action::Operate {
            op: Operator::Yank,
            target: Target::Motion(Motion::LineEnd),
            count: 1,
            sink: Sink::Ring,
        }));
        assert!(!lit(&ed).is_empty());

        ed.apply(cmd(Action::Move(Motion::Left)));
        assert!(lit(&ed).is_empty(), "the next thing you do puts it out");
    }

    #[test]
    fn a_flash_of_no_time_at_all_is_the_spelling_of_off() {
        let mut ed = editor("alpha\n");
        ex(&mut ed, "set yank_flash 0");

        ed.apply(cmd(Action::Operate {
            op: Operator::Yank,
            target: Target::Motion(Motion::CurrentLine),
            count: 1,
            sink: Sink::Ring,
        }));

        assert!(lit(&ed).is_empty());
        assert_eq!(ed.redraw_in(), None, "and nothing to wake up for");
    }

    #[test]
    fn an_expired_flash_draws_nothing_and_asks_for_no_more_frames() {
        let mut ed = editor("alpha\n");
        ex(&mut ed, "set yank_flash 1");
        ed.apply(cmd(Action::Operate {
            op: Operator::Yank,
            target: Target::Motion(Motion::CurrentLine),
            count: 1,
            sink: Sink::Ring,
        }));
        assert!(ed.redraw_in().is_some(), "a frame is owed while it is lit");

        std::thread::sleep(std::time::Duration::from_millis(20));

        assert!(lit(&ed).is_empty());
        assert_eq!(ed.redraw_in(), None, "and the loop goes back to blocking");
    }

    // ---- surround -----------------------------------------------------------

    fn surrounded(text: &str, at: usize, action: Action) -> (String, usize) {
        let mut ed = editor(text);
        ed.set_cursor(Cursor::at(at));
        ed.apply(cmd(action));
        (ed.buffer().unwrap().rope().to_string(), ed.cursor().unwrap().at)
    }

    #[test]
    fn ys_wraps_what_the_motion_covered() {
        let word = Target::Object { object: TextObject::Word { big: false }, around: false };
        assert_eq!(
            surrounded("hello there", 0, Action::Surround { target: word, count: 1, with: '"' }).0,
            "\"hello\" there"
        );
        // The open side adds a space inside; the close side does not.
        assert_eq!(
            surrounded("hello", 0, Action::Surround { target: word, count: 1, with: '{' }).0,
            "{ hello }"
        );
        assert_eq!(
            surrounded("hello", 0, Action::Surround { target: word, count: 1, with: '}' }).0,
            "{hello}"
        );
    }

    #[test]
    fn yss_wraps_the_line_without_its_terminator() {
        let line = Target::Motion(Motion::CurrentLine);
        let (text, _) =
            surrounded("alpha\nbeta\n", 0, Action::Surround { target: line, count: 1, with: ')' });
        assert_eq!(text, "(alpha)\nbeta\n");
    }

    #[test]
    fn ds_takes_the_delimiters_and_leaves_what_was_between_them() {
        assert_eq!(
            surrounded("say \"hello\" now", 6, Action::Unsurround { of: '"' }).0,
            "say hello now"
        );
        assert_eq!(surrounded("f( x )", 3, Action::Unsurround { of: '(' }).0, "f x ");
        assert_eq!(surrounded("f(x)", 2, Action::Unsurround { of: 'b' }).0, "fx", "`b` is `)`");
    }

    #[test]
    fn ds_takes_the_innermost_pair() {
        assert_eq!(surrounded("f(g(x))", 4, Action::Unsurround { of: '(' }).0, "f(gx)");
    }

    #[test]
    fn ds_with_nothing_around_the_cursor_changes_nothing_and_says_so() {
        let mut ed = editor("plain text\n");
        ed.apply(cmd(Action::Unsurround { of: '"' }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "plain text\n");
        assert_eq!(ed.session.status, "no \" around the cursor");
    }

    /// The whole reason `cs` is worth a command: the cursor does not move.
    #[test]
    fn cs_changes_the_pair_in_place_and_leaves_the_cursor_alone() {
        let (text, at) =
            surrounded("say \"hello\" now", 7, Action::Resurround { of: '"', with: '\'' });
        assert_eq!(text, "say 'hello' now");
        assert_eq!(at, 7, "still on the same character");
    }

    #[test]
    fn cs_to_an_open_bracket_adds_the_spaces_it_promises() {
        assert_eq!(surrounded("f(x)", 2, Action::Resurround { of: ')', with: '{' }).0, "f{ x }");
    }

    /// The last line of a file that does not end in a newline is where a
    /// linewise range reaches *backwards* for its terminator, and a surround
    /// must not follow it there.
    #[test]
    fn yss_on_the_last_unterminated_line_stays_on_it() {
        let line = Target::Motion(Motion::CurrentLine);
        let (text, _) =
            surrounded("alpha\nbeta", 7, Action::Surround { target: line, count: 1, with: ')' });
        assert_eq!(text, "alpha\n(beta)");
    }

    #[test]
    fn a_surround_is_one_undo_step() {
        let mut ed = editor("hello");
        ed.apply(cmd(Action::Surround {
            target: Target::Object { object: TextObject::Word { big: false }, around: false },
            count: 1,
            with: '"',
        }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "\"hello\"");

        ed.apply(cmd(Action::Undo));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "hello", "both edits, one step");
    }

    /// A rectangle wraps each row's columns. It used to wrap everything
    /// between the two corners — brackets, line ends and all — because
    /// `SurroundSelection` had no blockwise arm and a charwise range was the
    /// only thing it knew how to build.
    #[test]
    fn visual_s_over_a_rectangle_wraps_every_row() {
        let mut ed = visual("let alpha = 1;\nlet beta  = 2;\n", 4, Shape::Block);
        for _ in 0..4 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }
        ed.apply(cmd(Action::Move(Motion::Down)));

        ed.apply(cmd(Action::SurroundSelection { with: '"' }));

        assert_eq!(whole(&ed), "let \"alpha\" = 1;\nlet \"beta \" = 2;\n");
    }

    /// And one `u` takes the whole rectangle back.
    #[test]
    fn visual_s_over_a_rectangle_is_one_undo_step() {
        let text = "ab\ncd\n";
        let mut ed = visual(text, 0, Shape::Block);
        ed.apply(cmd(Action::Move(Motion::Down)));

        ed.apply(cmd(Action::SurroundSelection { with: '"' }));
        assert_eq!(whole(&ed), "\"a\"b\n\"c\"d\n");

        ed.apply(cmd(Action::Undo));
        assert_eq!(whole(&ed), text);
    }

    #[test]
    fn visual_s_wraps_the_selection_and_leaves_visual_mode() {
        let mut ed = editor("hello there");
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
        for _ in 0..4 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }
        ed.apply(cmd(Action::SurroundSelection { with: '"' }));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "\"hello\" there");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    // ---- :case --------------------------------------------------------------

    #[test]
    fn case_respells_the_word_under_the_cursor() {
        let mut ed = editor("let hello_world = 1;\n");
        ed.set_cursor(Cursor::at(6));

        ex(&mut ed, "case camel");

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "let helloWorld = 1;\n");
        assert_eq!(ed.cursor().unwrap().at, 4, "on the first character of the name");
    }

    #[test]
    fn case_respells_a_selection_and_leaves_visual_mode() {
        let mut ed = editor("one_two three_four\n");
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
        for _ in 0..6 {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }

        ex(&mut ed, "case pascal");

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "OneTwo three_four\n");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn case_is_one_undo_step() {
        let mut ed = editor("hello_world\n");
        ex(&mut ed, "case const");
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "HELLO_WORLD\n");

        ed.apply(cmd(Action::Undo));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "hello_world\n");
    }

    #[test]
    fn case_with_no_style_or_a_bad_one_says_what_it_takes() {
        let mut ed = editor("hello_world\n");
        ex(&mut ed, "case sideways");

        assert!(
            ed.session.status.starts_with("case what? one of upper, lower"),
            "{}",
            ed.session.status
        );
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "hello_world\n", "and nothing changed");
    }

    #[test]
    fn case_on_nothing_says_so() {
        let mut ed = editor("   \n");
        ed.set_cursor(Cursor::at(1));
        ex(&mut ed, "case snake");
        assert_eq!(ed.session.status, "no word under the cursor");
    }

    /// Every cursor gets it, which is what makes `Ctrl-N` and `:case` a rename.
    #[test]
    fn case_reaches_every_cursor() {
        let mut ed = editor("foo_bar\nfoo_bar\n");
        ed.apply(cmd(Action::AddCursorLine { below: true }));

        ex(&mut ed, "case camel");

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "fooBar\nfooBar\n");
    }

    // ---- labels -------------------------------------------------------------

    /// The letter `Ctrl-W f` put on one window.
    fn label_of_window(ed: &Editor, id: WindowId) -> String {
        ed.session
            .labels
            .as_ref()
            .expect("letters are up")
            .targets
            .iter()
            .find(|(_, target)| *target == LabelTarget::Window(id))
            .map(|(label, _)| label.clone())
            .expect("that window has a letter")
    }

    /// Two windows, so there is something to pick between.
    fn two_windows(text: &str) -> Editor {
        let mut ed = editor(text);
        split(&mut ed, Dir::Vertical);
        ed
    }

    #[test]
    fn ctrl_w_f_puts_a_letter_on_every_window_including_this_one() {
        let mut ed = two_windows("alpha\n");
        ed.apply(cmd(Action::Window(WindowCmd::Pick)));

        assert_eq!(ed.session.mode, Mode::Label);
        let labels = ed.session.labels.as_ref().expect("letters are up");
        assert_eq!(labels.targets.len(), 2, "the focused window gets one too");
        assert_eq!(labels.targets[0].0, "f", "the hand, not the alphabet");

        // And each one is drawn in its own window.
        for (label, target) in &labels.targets {
            let LabelTarget::Window(id) = target else { panic!("a window label") };
            let wanted = format!(" {label} ");
            let drawn = ed.decorations(*id, 0..1);
            assert!(
                drawn.iter().any(|d| matches!(d, Decoration::Overlay { text, .. }
                        if *text == wanted)),
                "{label} is not on its own window"
            );
        }
    }

    /// A single character in a corner is one more character on a screen full
    /// of them. The letter is a block you cannot miss, in the middle of the
    /// pane it belongs to.
    #[test]
    fn a_window_letter_is_a_block_in_the_middle_of_its_pane() {
        let mut ed = two_windows("one\ntwo\nthree\nfour\nfive\n");
        sized(&mut ed);
        let focus = ed.focus();
        ed.size_window(focus, 21, 5);
        ed.apply(cmd(Action::Window(WindowCmd::Pick)));

        let box_of: Vec<(usize, usize, String)> = ed
            .decorations(focus, 0..5)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Overlay { row, col, text, layer: Layer::Over, .. } => {
                    Some((row, col, text))
                }
                _ => None,
            })
            .collect();

        // Three rows tall, the letter in the middle one, every row the same
        // width and the same column — a block, not three loose strings.
        //
        // Row 2 of 0..5, and column 7: the pane is 21 wide with a three-cell
        // gutter for a five-line file — one reserved for signs and two for the
        // numbers — so the three-wide block starts at (18 - 3) / 2.
        let letter = label_of_window(&ed, focus);
        assert_eq!(
            box_of,
            [(1, 7, "   ".into()), (2, 7, format!(" {letter} ")), (3, 7, "   ".into())],
            "the letter is not a block in the middle"
        );
    }

    /// A file with fewer lines than the box has rows keeps the rows it has:
    /// the missing ones would land on the `~` filler, where there is no line
    /// to decorate.
    #[test]
    fn a_short_file_gets_as_much_of_the_block_as_it_has_room_for() {
        let mut ed = two_windows("only\n");
        sized(&mut ed);
        let focus = ed.focus();
        ed.size_window(focus, 21, 20);
        ed.apply(cmd(Action::Window(WindowCmd::Pick)));

        let rows: Vec<usize> = ed
            .decorations(focus, 0..1)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Overlay { row, .. } => Some(row),
                _ => None,
            })
            .collect();

        assert_eq!(rows, [0], "the one row there is, and no box drawn over nothing");
    }

    #[test]
    fn pressing_a_letter_goes_to_that_window() {
        let mut ed = two_windows("alpha\n");
        let here = ed.focus();
        ed.apply(cmd(Action::Window(WindowCmd::Pick)));
        let elsewhere = other(&ed);
        let label = ed
            .session
            .labels
            .as_ref()
            .unwrap()
            .targets
            .iter()
            .find(|(_, target)| *target == LabelTarget::Window(elsewhere))
            .map(|(label, _)| label.clone())
            .expect("the other window has a letter");

        ed.apply(cmd(Action::LabelChar(label.chars().next().unwrap())));

        assert_eq!(ed.focus(), elsewhere);
        assert_ne!(ed.focus(), here, "and it was the other one");
        assert_eq!(ed.session.mode, Mode::Normal, "the letters are gone");
        assert!(ed.session.labels.is_none());
    }

    #[test]
    fn a_key_that_is_no_label_cancels_and_so_does_esc() {
        let mut ed = two_windows("alpha\n");
        let here = ed.focus();

        ed.apply(cmd(Action::Window(WindowCmd::Pick)));
        ed.apply(cmd(Action::LabelChar('z')));
        assert_eq!(ed.session.mode, Mode::Normal, "a mistyped label costs one press");
        assert_eq!(ed.focus(), here);

        ed.apply(cmd(Action::Window(WindowCmd::Pick)));
        ed.apply(cmd(Action::LabelCancel));
        assert_eq!(ed.session.mode, Mode::Normal);
        assert_eq!(ed.focus(), here);
    }

    #[test]
    fn one_window_is_not_worth_a_mode() {
        let mut ed = editor("alpha\n");
        ed.apply(cmd(Action::Window(WindowCmd::Pick)));

        assert_eq!(ed.session.mode, Mode::Normal);
        assert_eq!(ed.session.status, "only one window");
    }

    // ---- `s`, find on screen ------------------------------------------------

    /// An editor with a screen, because `s` searches the viewport and a
    /// viewport needs a size.
    fn aiming(text: &str) -> Editor {
        let mut ed = editor(text);
        sized(&mut ed);
        // The frontend reports back how much of the pane is text; without it
        // the window is nought rows tall and nothing is on screen to aim at.
        let focus = ed.focus();
        ed.size_window(focus, 80, 20);
        ed.apply(cmd(Action::EnterFind));
        ed
    }

    fn type_find(ed: &mut Editor, keys: &str) {
        for c in keys.chars() {
            ed.apply(cmd(Action::FindChar(c)));
        }
    }

    fn label_of(ed: &Editor, at: usize) -> String {
        ed.session
            .labels
            .as_ref()
            .expect("letters are up")
            .targets
            .iter()
            .find(|(_, target)| *target == LabelTarget::Position(at))
            .map(|(label, _)| label.clone())
            .unwrap_or_else(|| panic!("no letter on the match at {at}"))
    }

    #[test]
    fn typing_narrows_what_is_matched() {
        let mut ed = aiming("fun fur function\n");
        type_find(&mut ed, "fu");
        assert_eq!(ed.session.find.as_ref().unwrap().matches.len(), 3);

        type_find(&mut ed, "n");
        assert_eq!(ed.session.find.as_ref().unwrap().matches.len(), 2, "`fur` is out");
    }

    /// The one rule that lets typing and jumping share a keyboard.
    #[test]
    fn no_letter_is_a_character_that_could_narrow_the_search() {
        let mut ed = aiming("fun function funny\n");
        type_find(&mut ed, "fun");

        let labels = ed.session.labels.as_ref().unwrap();
        // ` `, `c` and `n` all continue a match on screen, so none of them can
        // be a letter — pressing them has to mean "narrow".
        for (label, _) in &labels.targets {
            assert!(
                !label.contains('c') && !label.contains('n') && !label.contains('C'),
                "{label} would swallow a keystroke that means something else"
            );
        }
    }

    #[test]
    fn pressing_a_letter_lands_on_the_first_character_of_that_match() {
        let mut ed = aiming("alpha beta alpha\n");
        type_find(&mut ed, "alpha");
        let label = label_of(&ed, 11);

        for c in label.chars() {
            ed.apply(cmd(Action::FindChar(c)));
        }

        assert_eq!(ed.cursor().unwrap().at, 11, "the start of the second one");
        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.session.find.is_none(), "and the dimming is gone with it");
    }

    #[test]
    fn a_query_that_matches_nothing_leaves_and_so_does_esc() {
        let mut ed = aiming("alpha\n");
        let here = ed.cursor().unwrap().at;
        type_find(&mut ed, "zz");
        assert_eq!(ed.session.mode, Mode::Normal, "nothing to press, nothing to narrow");
        assert_eq!(ed.cursor().unwrap().at, here);

        let mut ed = aiming("alpha\n");
        type_find(&mut ed, "al");
        ed.apply(cmd(Action::FindCancel));
        assert_eq!(ed.session.mode, Mode::Normal);
        assert_eq!(ed.cursor().unwrap().at, here, "and the cursor never moved");
    }

    #[test]
    fn backspace_narrows_back_and_then_leaves() {
        let mut ed = aiming("fun fur\n");
        type_find(&mut ed, "fun");
        assert_eq!(ed.session.find.as_ref().unwrap().matches.len(), 1);

        ed.apply(cmd(Action::FindBackspace));
        assert_eq!(ed.session.find.as_ref().unwrap().matches.len(), 2, "`fur` is back");

        ed.apply(cmd(Action::FindBackspace));
        ed.apply(cmd(Action::FindBackspace));
        assert_eq!(ed.session.mode, Mode::Find, "an empty query still aims");
        ed.apply(cmd(Action::FindBackspace));
        assert_eq!(ed.session.mode, Mode::Normal, "and one more leaves");
    }

    /// A jump is aimed at what you can see. Something below the fold is not
    /// somewhere you are aiming, and a letter on it could not be drawn.
    #[test]
    fn only_the_viewport_is_searched() {
        let mut ed = editor(&format!("alpha\n{}alpha\n", "\n".repeat(80)));
        sized(&mut ed);
        let focus = ed.focus();
        ed.size_window(focus, 80, 20);
        ed.apply(cmd(Action::EnterFind));
        type_find(&mut ed, "alpha");

        let matches = &ed.session.find.as_ref().unwrap().matches;
        assert_eq!(matches.len(), 1, "the one on screen");
        assert_eq!(matches[0].0, 0);
    }

    #[test]
    fn smartcase_is_the_same_rule_the_search_line_follows() {
        let mut ed = aiming("Fn fn\n");
        type_find(&mut ed, "fn");
        assert_eq!(ed.session.find.as_ref().unwrap().matches.len(), 2, "lowercase finds both");

        let mut ed = aiming("Fn fn\n");
        type_find(&mut ed, "Fn");
        assert_eq!(ed.session.find.as_ref().unwrap().matches.len(), 1, "a capital means it");
    }

    #[test]
    fn what_is_on_screen_while_aiming() {
        let mut ed = aiming("alpha beta\n");
        type_find(&mut ed, "beta");

        let drawn = ed.decorations(ed.focus(), 0..1);
        let ui = &ed.theme().ui;
        assert!(
            drawn
                .iter()
                .any(|d| matches!(d, Decoration::Repaint { style, .. } if *style == ui.dim)),
            "everything else dims"
        );
        assert!(
            drawn.iter().any(
                |d| matches!(d, Decoration::Repaint { range, style, .. } if *range == (6..10) && *style == ui.search)
            ),
            "and the match lights up"
        );
        assert!(
            drawn.iter().any(|d| matches!(d, Decoration::Inline { row: 0, col: 10, .. })),
            "with its letter after it, in a cell of its own"
        );
    }

    /// The dim is the announcement that `s` is aiming. One that waits for the
    /// first letter leaves you looking at an ordinary screen, wondering
    /// whether the key registered.
    #[test]
    fn the_screen_dims_the_moment_s_is_pressed() {
        let ed = aiming("alpha beta\n");
        let dim = ed.theme().ui.dim;
        assert!(
            ed.decorations(ed.focus(), 0..1)
                .iter()
                .any(|d| matches!(d, Decoration::Repaint { style, .. } if *style == dim)),
            "before a single character of the query"
        );
    }

    // ---- `S`, select by structure -------------------------------------------

    /// A buffer with a real grammar behind it, which `S` needs and
    /// `Editor::empty()` has no path to get.
    fn parsed(name: &str, text: &str) -> (Scratch, Editor) {
        let f = Scratch::new(name, text);
        let mut ed = opened(&f);
        sized(&mut ed);
        let focus = ed.focus();
        ed.size_window(focus, 80, 20);
        (f, ed)
    }

    fn scopes(ed: &Editor) -> Vec<(String, (usize, usize))> {
        ed.session
            .labels
            .as_ref()
            .expect("letters are up")
            .targets
            .iter()
            .filter_map(|(label, target)| match target {
                LabelTarget::Scope(start, end) => Some((label.clone(), (*start, *end))),
                _ => None,
            })
            .collect()
    }

    /// TODO.md's own example, in the language it was written in.
    ///
    /// `{ "hello/plugin" },` with the cursor inside the string wants three
    /// scopes: the contents, the string, the table. Which is exactly the chain
    /// of nodes the Lua grammar puts there — no special case for strings or
    /// brackets anywhere.
    #[test]
    fn the_scopes_around_a_string_are_its_contents_then_it_then_the_table() {
        let (_f, mut ed) = parsed("scopes.lua", "return { \"hello/plugin\" },\n");
        ed.set_cursor(Cursor::at(12)); // inside `hello/plugin`

        ed.apply(cmd(Action::ShowScopes));

        let found = scopes(&ed);
        let text = |(start, end): (usize, usize)| ed.buffer().unwrap().slice(start, end);
        assert_eq!(found[0].0, "a");
        assert_eq!(text(found[0].1), "hello/plugin", "the contents, tightest first");
        assert_eq!(found[1].0, "b");
        assert_eq!(text(found[1].1), "\"hello/plugin\"", "the string, quotes and all");
        assert_eq!(found[2].0, "c");
        assert_eq!(text(found[2].1), "{ \"hello/plugin\" }", "the table");
    }

    #[test]
    fn both_ends_of_every_scope_carry_the_same_letter() {
        let (_f, mut ed) = parsed("ends.lua", "return { \"hi\" },\n");
        ed.set_cursor(Cursor::at(10));
        ed.apply(cmd(Action::ShowScopes));

        let (label, (start, end)) = scopes(&ed)[0].clone();
        let drawn = inline(&ed);

        let mine: Vec<usize> =
            drawn.iter().filter(|(_, text)| *text == label).map(|&(col, _)| col).collect();
        assert_eq!(mine.len(), 2, "one letter, both ends");
        assert!(mine.contains(&start), "in front of the first character of the scope");
        assert!(mine.contains(&end), "and after its last one");
    }

    /// Row 0 with its letters threaded in, the way the renderer inserts them:
    /// each label in front of the column it names, and two at one column in
    /// the order they were produced.
    fn lettered(ed: &Editor, text: &str) -> String {
        let width = text.chars().count();
        let mut before = vec![String::new(); width + 1];
        for (col, label) in inline(ed) {
            before[col].push_str(&label);
        }
        text.chars()
            .enumerate()
            .map(|(i, c)| format!("{}{c}", before[i]))
            .chain(std::iter::once(before[width].clone()))
            .collect()
    }

    /// The picture in `docs/specs/scopes.md`, built out of what the
    /// decorations actually say. Every character of the line is still there
    /// and the letters are between them.
    #[test]
    fn the_letters_thread_through_the_line_rather_than_over_it() {
        let text = "return { \"hello/plugin\" },";
        let (_f, mut ed) = parsed("picture.lua", &format!("{text}\n"));
        ed.set_cursor(Cursor::at(12)); // inside `hello/plugin`
        ed.apply(cmd(Action::ShowScopes));

        let drawn = lettered(&ed, text);

        assert!(
            drawn.contains("c{ b\"ahello/plugina\"b }c"),
            "the spec's own picture is not what is drawn: {drawn}"
        );
        let bare: String = drawn.chars().filter(|c| !c.is_ascii_lowercase()).collect();
        let stripped: String = text.chars().filter(|c| !c.is_ascii_lowercase()).collect();
        assert_eq!(bare, stripped, "and nothing of the line was dropped to make room");
    }

    /// Every inline label on row 0, as the renderer takes them: column and
    /// text, in the order they were produced.
    fn inline(ed: &Editor) -> Vec<(usize, String)> {
        ed.decorations(ed.focus(), 0..1)
            .into_iter()
            .filter_map(|d| match d {
                Decoration::Inline { row: 0, col, text, .. } => Some((col, text)),
                _ => None,
            })
            .collect()
    }

    /// Two scopes ending in the same place are two cells, not one letter on
    /// top of another — the whole point of the letters is seeing how much you
    /// are about to select.
    #[test]
    fn scopes_sharing_an_edge_get_a_cell_each() {
        let (_f, mut ed) = parsed("share.lua", "return { \"hi\" },\n");
        ed.set_cursor(Cursor::at(10));
        ed.apply(cmd(Action::ShowScopes));

        let found = scopes(&ed);
        let drawn = inline(&ed);
        // The string and its contents end one character apart, and the string
        // and the table start one apart: nothing here may be dropped.
        for (label, _) in &found {
            assert_eq!(
                drawn.iter().filter(|(_, text)| text == label).count(),
                2,
                "{label} lost an end"
            );
        }

        // Where two do share a column, the closing letters come innermost
        // first and the opening ones outermost first, so the whole list nests
        // the way brackets do.
        let ends: Vec<&String> =
            drawn.iter().filter(|&&(col, _)| col == found[0].1.1).map(|(_, text)| text).collect();
        assert!(ends.len() <= 2 && ends.first().is_some_and(|first| *first == &found[0].0));
    }

    #[test]
    fn pressing_a_letter_selects_that_scope() {
        let (_f, mut ed) = parsed("select.lua", "return { \"hi\" },\n");
        ed.set_cursor(Cursor::at(10));
        ed.apply(cmd(Action::ShowScopes));
        let (label, (start, end)) = scopes(&ed)[1].clone();

        ed.apply(cmd(Action::LabelChar(label.chars().next().unwrap())));

        assert_eq!(ed.session.mode, Mode::Visual(Shape::Chars));
        let selection = ed.selections().unwrap().primary();
        assert_eq!(selection.anchor.at, start);
        assert_eq!(selection.head.at, end - 1, "charwise visual sits *on* the last character");
    }

    #[test]
    fn a_file_with_no_grammar_has_no_structure_to_offer() {
        let (_f, mut ed) = parsed("plain.unknownext", "some words here\n");

        ed.apply(cmd(Action::ShowScopes));

        assert_eq!(ed.session.status, "no parse tree for this file");
        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.session.labels.is_none());
    }

    #[test]
    fn a_key_that_is_no_scope_cancels_and_leaves_the_cursor_alone() {
        let (_f, mut ed) = parsed("cancel.lua", "return { \"hi\" },\n");
        ed.set_cursor(Cursor::at(10));
        ed.apply(cmd(Action::ShowScopes));

        ed.apply(cmd(Action::LabelChar('9')));

        assert_eq!(ed.session.mode, Mode::Normal);
        assert_eq!(ed.cursor().unwrap().at, 10);
    }

    // ---- Ctrl-P, the file picker --------------------------------------------

    #[test]
    fn the_file_picker_lists_what_is_under_the_root_and_opens_what_you_choose() {
        let files = Files::new("picker");
        files.file("alpha.rs", "one\n");
        let path = files.file("beta.rs", "two\n");
        let mut ed = Editor::open(&path).unwrap();
        ed.session.tree_root = Some(files.0.clone());

        ed.apply(cmd(Action::OpenPicker(PickerKind::File)));
        assert_eq!(ed.session.mode, Mode::Pick);
        let listed: Vec<String> =
            ed.session.picker.as_ref().unwrap().items().iter().map(|i| i.text.clone()).collect();
        assert_eq!(listed, ["alpha.rs", "beta.rs"]);

        // Type enough to name the other one, and take it.
        ed.apply(cmd(Action::PickChar('a')));
        ed.apply(cmd(Action::PickChar('l')));
        ed.apply(cmd(Action::PickAccept));

        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\n");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    #[test]
    fn choosing_a_file_that_is_already_open_reuses_its_buffer() {
        let files = Files::new("picker-reuse");
        let path = files.file("alpha.rs", "one\n");
        let mut ed = Editor::open(&path).unwrap();
        ed.session.tree_root = Some(files.0.clone());
        let before = ed.buffer_ids().len();

        ed.apply(cmd(Action::OpenPicker(PickerKind::File)));
        ed.apply(cmd(Action::PickAccept));

        assert_eq!(ed.buffer_ids().len(), before, "one file, one buffer");
    }

    /// The picker reached from a tree opens the file the way Enter on a tree
    /// row does — in the window you came from, sidebar intact. Opening it over
    /// the tree would close the thing you were looking a file up in.
    #[test]
    fn a_file_picked_from_a_tree_lands_beside_it_and_not_on_it() {
        let d = ScratchDir::new("picker-tree").file("a.rs");
        let mut ed = editor("one");
        sized(&mut ed);
        let file = ed.focus();
        ex(&mut ed, &format!("vs {}", d.path()));
        let tree = ed.focus();
        ed.session.tree_root = Some(std::path::PathBuf::from(d.path()));

        ed.apply(cmd(Action::OpenPicker(PickerKind::File)));
        ed.apply(cmd(Action::PickAccept));

        assert!(ed.window_of(tree).unwrap().tree().is_some(), "the sidebar stayed");
        assert_eq!(ed.focus(), file, "and the file landed where you came from");
        assert!(ed.name_of(ed.window().buffer().unwrap()).ends_with("a.rs"));
    }

    #[test]
    fn a_root_with_no_files_says_so_rather_than_opening_an_empty_overlay() {
        let files = Files::new("picker-empty");
        let mut ed = Editor::empty();
        ed.session.tree_root = Some(files.0.clone());

        ed.apply(cmd(Action::OpenPicker(PickerKind::File)));

        assert_eq!(ed.session.mode, Mode::Normal);
        assert!(ed.session.status.starts_with("no files under"), "{}", ed.session.status);
    }

    // ---- :alt ---------------------------------------------------------------

    #[test]
    fn alt_opens_the_test_beside_the_implementation_and_back() {
        let files = Files::new("alt");
        let source = files.file("thing.go", "package main\n");
        files.file("thing_test.go", "package main\n");
        let mut ed = Editor::open(&source).unwrap();

        ex(&mut ed, "alt");
        assert!(ed.name_of(ed.buffer_ids()[1]).ends_with("thing_test.go"));
        assert!(ed.buffer().unwrap().path.as_ref().unwrap().ends_with("thing_test.go"));

        ex(&mut ed, "alt");
        assert!(
            ed.buffer().unwrap().path.as_ref().unwrap().ends_with("thing.go"),
            "and `*_test.go` is tried before `*.go`, or a test is its own alternate"
        );
    }

    #[test]
    fn alt_takes_the_first_of_its_paths_that_is_there() {
        let files = Files::new("alt-order");
        let source = files.file("main.cpp", "int main() {}\n");
        // `*.hpp` is offered first and does not exist; `*.h` does.
        files.file("main.h", "#pragma once\n");
        let mut ed = Editor::open(&source).unwrap();

        ex(&mut ed, "alt");

        assert!(ed.buffer().unwrap().path.as_ref().unwrap().ends_with("main.h"));
    }

    #[test]
    fn alt_with_nothing_to_open_says_which_names_it_looked_for() {
        let files = Files::new("alt-missing");
        let source = files.file("lonely.go", "package main\n");
        let mut ed = Editor::open(&source).unwrap();

        ex(&mut ed, "alt");

        assert!(ed.session.status.starts_with("none of "), "{}", ed.session.status);
        assert!(ed.session.status.contains("lonely_test.go"), "{}", ed.session.status);
    }

    #[test]
    fn alt_on_a_file_no_rule_matches_says_so() {
        let files = Files::new("alt-unmatched");
        let source = files.file("notes.md", "# hi\n");
        let mut ed = Editor::open(&source).unwrap();

        ex(&mut ed, "alt");

        assert!(ed.session.status.starts_with("no alternate for"), "{}", ed.session.status);
    }

    /// A rule you write replaces bi's for that pattern rather than sitting
    /// beside it, so there is never a question of which one won.
    #[test]
    fn a_rule_in_the_config_replaces_the_built_in_one() {
        let files = Files::new("alt-config");
        let source = files.file("thing.go", "package main\n");
        files.file("thing.pb.go", "// generated\n");
        let mut ed = Editor::open(&source).unwrap();

        ed.load_config(ConfigText(Some("[alternate]\n\"*.go\" = [\"*.pb.go\"]\n")));
        ex(&mut ed, "alt");

        assert!(ed.buffer().unwrap().path.as_ref().unwrap().ends_with("thing.pb.go"));
    }

    // ---- the buffer switcher ------------------------------------------------

    fn names(ed: &Editor) -> Vec<String> {
        ed.session
            .picker
            .as_ref()
            .expect("the list is up")
            .items()
            .iter()
            .map(|i| {
                std::path::Path::new(&i.text)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| i.text.clone())
            })
            .collect()
    }

    /// Three files, opened in order, so the list has something to sort.
    fn three(tag: &str) -> (Files, Editor) {
        let files = Files::new(tag);
        let a = files.file("a.txt", "a\n");
        let mut ed = Editor::open(&a).unwrap();
        for name in ["b.txt", "c.txt"] {
            let path = files.file(name, "x\n");
            ex(&mut ed, &format!("e {path}"));
        }
        (files, ed)
    }

    #[test]
    fn the_switcher_lists_the_most_recently_shown_first() {
        let (_files, mut ed) = three("mru");
        ed.apply(cmd(Action::Buffer(BufferCmd::List)));

        assert_eq!(names(&ed), ["c.txt", "b.txt", "a.txt"]);
    }

    /// The rows are paths relative to the session's root — what you would
    /// have typed, the file picker's rule. A path outside the root stays
    /// whole rather than sprouting `../`s.
    #[test]
    fn the_switcher_shows_paths_relative_to_the_root() {
        let (files, mut ed) = three("mru-relative");
        ed.session.tree_root = Some(files.0.clone());
        ed.apply(cmd(Action::Buffer(BufferCmd::List)));

        let rows: Vec<&str> = ed
            .session
            .picker
            .as_ref()
            .expect("the list is up")
            .items()
            .iter()
            .map(|i| i.text.as_str())
            .collect();
        assert_eq!(rows, ["c.txt", "b.txt", "a.txt"], "no absolute prefixes");
    }

    /// Opened *and taken* is a switch: the row it starts on is the buffer you
    /// were in before this one.
    #[test]
    fn the_switcher_opens_on_the_previous_buffer() {
        let (_files, mut ed) = three("mru-open");
        ed.apply(cmd(Action::Buffer(BufferCmd::List)));
        ed.apply(cmd(Action::PickAccept));

        assert!(ed.name_of(ed.window().buffer().unwrap()).ends_with("b.txt"));

        // And again, which is what makes it a toggle.
        ed.apply(cmd(Action::Buffer(BufferCmd::List)));
        ed.apply(cmd(Action::PickAccept));
        assert!(ed.name_of(ed.window().buffer().unwrap()).ends_with("c.txt"));
    }

    #[test]
    fn typing_leaves_the_default_row_and_matches_a_subsequence() {
        let (_files, mut ed) = three("mru-typing");
        ed.apply(cmd(Action::Buffer(BufferCmd::List)));

        ed.apply(cmd(Action::PickChar('a')));
        ed.apply(cmd(Action::PickChar('t')));
        ed.apply(cmd(Action::PickAccept));

        assert!(
            ed.name_of(ed.window().buffer().unwrap()).ends_with("a.txt"),
            "`at` is a subsequence of a.txt and of nothing else here"
        );
    }

    /// The editor's half of LSP: attachment in `settle`, the drain feeding
    /// `didChange`, stored diagnostics, and `:lsp`. The protocol itself is
    /// tested in `src/lsp/`; here the server is a fake and the interest is
    /// the wiring. See `docs/specs/lsp.md`.
    mod lsp_integration {
        use serde_json::json;

        use super::*;
        use crate::lsp::transport::fake::FakeSpawn;
        use crate::lsp::{Inbound, Severity};

        /// A project on disk, an editor opened on its main file, and a fake
        /// behind the registry — installed before the first settle, which is
        /// when attachment happens.
        fn project(name: &str) -> (ScratchDir, Editor, FakeSpawn) {
            let dir = ScratchDir::new(&format!("lsp-{name}"))
                .written("Cargo.toml", "[package]\n")
                .written("src/main.rs", "fn main() {\n}\n");
            let mut ed = Editor::open(format!("{}/src/main.rs", dir.path())).unwrap();
            let fake = FakeSpawn::default();
            ed.set_lsp_spawner(fake.clone());
            (dir, ed, fake)
        }

        /// Settle to attach, grant the handshake, settle to open.
        fn handshake(ed: &mut Editor, fake: &FakeSpawn) -> crate::lsp::ServerId {
            ed.settle();
            let id = fake.spawned.lock().unwrap()[0].0;
            fake.grant(
                id,
                json!({ "positionEncoding": "utf-8",
                        "textDocumentSync": { "openClose": true, "change": 2, "save": true },
                        "definitionProvider": true,
                        "declarationProvider": true,
                        "implementationProvider": true,
                        "referencesProvider": true,
                        "documentFormattingProvider": true,
                        "hoverProvider": true,
                        "completionProvider": { "triggerCharacters": ["."] },
                        "signatureHelpProvider": { "triggerCharacters": ["(", ","] } }),
            );
            ed.settle();
            id
        }

        /// Answers the newest request of `method` as the server, and settles
        /// so the answer lands.
        fn respond(
            ed: &mut Editor,
            fake: &FakeSpawn,
            server: crate::lsp::ServerId,
            method: &str,
            result: serde_json::Value,
        ) {
            let id = fake.last(server, method).unwrap()["id"].as_i64().unwrap();
            let inbox = fake.spawned.lock().unwrap()[0].1.clone();
            inbox.deliver(server, Inbound::Response { id, result: Ok(result) });
            ed.settle();
        }

        fn open_uri(fake: &FakeSpawn, server: crate::lsp::ServerId) -> String {
            fake.last(server, "textDocument/didOpen").unwrap()["params"]["textDocument"]["uri"]
                .as_str()
                .unwrap()
                .to_string()
        }

        fn publish(
            ed: &mut Editor,
            fake: &FakeSpawn,
            id: crate::lsp::ServerId,
            params: serde_json::Value,
        ) {
            let inbox = fake.spawned.lock().unwrap()[0].1.clone();
            inbox.deliver(
                id,
                Inbound::Notification { method: "textDocument/publishDiagnostics".into(), params },
            );
            ed.settle();
        }

        #[test]
        fn opening_a_rust_file_attaches_and_opens_after_the_handshake() {
            let (_dir, mut ed, fake) = project("attach");
            let id = handshake(&mut ed, &fake);

            assert_eq!(
                fake.methods(id),
                ["initialize", "initialized", "textDocument/didOpen"],
                "the protocol's own order, driven entirely by settle"
            );
            let open = fake.last(id, "textDocument/didOpen").unwrap();
            assert_eq!(open["params"]["textDocument"]["text"], "fn main() {\n}\n");
            assert_eq!(open["params"]["textDocument"]["languageId"], "rust");

            ex(&mut ed, "lsp");
            let status = &ed.session.status;
            assert!(status.contains("rust-analyzer: running"), "{status}");
            assert!(status.contains("utf-8"), "{status}");
            assert!(status.contains("0 diagnostics"), "{status}");
        }

        #[test]
        fn text_typed_while_the_server_starts_rides_the_did_open() {
            let (_dir, mut ed, fake) = project("early");
            ed.settle();
            let id = fake.spawned.lock().unwrap()[0].0;

            // Typed before the handshake answered: no didChange can be sent,
            // and none is needed — the open carries the current text.
            ed.buffer_mut().unwrap().insert_str(Cursor::at(0), "x");
            ed.settle();

            fake.grant(id, json!({ "textDocumentSync": 2 }));
            ed.settle();
            assert_eq!(fake.methods(id), ["initialize", "initialized", "textDocument/didOpen"]);
            let open = fake.last(id, "textDocument/didOpen").unwrap();
            assert_eq!(open["params"]["textDocument"]["text"], "xfn main() {\n}\n");
        }

        #[test]
        fn an_edit_becomes_one_did_change_through_the_same_drain_as_the_parse_tree() {
            let (_dir, mut ed, fake) = project("change");
            let id = handshake(&mut ed, &fake);

            ed.buffer_mut().unwrap().insert_str(Cursor::at(3), "x");
            ed.settle();

            let change = fake.last(id, "textDocument/didChange").unwrap();
            assert_eq!(change["params"]["textDocument"]["version"], 2);
            let c = &change["params"]["contentChanges"][0];
            assert_eq!(c["range"]["start"], json!({ "line": 0, "character": 0 }));
            assert_eq!(c["range"]["end"], json!({ "line": 1, "character": 0 }));
            assert_eq!(c["text"], "fn xmain() {\n");
        }

        #[test]
        fn diagnostics_are_stored_as_char_offsets_and_follow_edits() {
            let (_dir, mut ed, fake) = project("diag");
            let id = handshake(&mut ed, &fake);
            let buffer = ed.window().buffer().unwrap();
            let uri =
                fake.last(id, "textDocument/didOpen").unwrap()["params"]["textDocument"]["uri"]
                    .as_str()
                    .unwrap()
                    .to_string();

            publish(
                &mut ed,
                &fake,
                id,
                json!({
                    "uri": uri, "version": 1,
                    "diagnostics": [{ "range": { "start": { "line": 0, "character": 3 },
                                                 "end": { "line": 0, "character": 7 } },
                                      "severity": 1, "source": "rustc", "message": "nope" }]
                }),
            );

            let stored = ed.diagnostics(buffer);
            assert_eq!((stored[0].start, stored[0].end), (3, 7));
            assert_eq!(stored[0].severity, Severity::Error);
            assert_eq!(stored[0].message, "nope");

            // An edit above shifts them, exactly as it shifts a selection.
            ed.buffer_mut().unwrap().insert_str(Cursor::at(0), "x");
            ed.settle();
            let stored = ed.diagnostics(buffer);
            assert_eq!((stored[0].start, stored[0].end), (4, 8));

            ex(&mut ed, "lsp");
            assert!(ed.session.status.contains("· 1 diagnostic"), "{}", ed.session.status);
        }

        #[test]
        fn diagnostics_for_a_version_that_is_gone_are_dropped() {
            let (_dir, mut ed, fake) = project("stale");
            let id = handshake(&mut ed, &fake);
            let buffer = ed.window().buffer().unwrap();
            let uri =
                fake.last(id, "textDocument/didOpen").unwrap()["params"]["textDocument"]["uri"]
                    .as_str()
                    .unwrap()
                    .to_string();

            // The edit moves the document to version 2; version-1 findings
            // describe text that no longer exists.
            ed.buffer_mut().unwrap().insert_str(Cursor::at(0), "x");
            ed.settle();
            publish(
                &mut ed,
                &fake,
                id,
                json!({
                    "uri": uri, "version": 1,
                    "diagnostics": [{ "range": { "start": { "line": 0, "character": 0 },
                                                 "end": { "line": 0, "character": 1 } },
                                      "message": "old news" }]
                }),
            );
            assert!(ed.diagnostics(buffer).is_empty());
        }

        #[test]
        fn a_write_becomes_did_save() {
            let (_dir, mut ed, fake) = project("save");
            let id = handshake(&mut ed, &fake);

            ex(&mut ed, "w");
            ed.settle();
            let save = fake.last(id, "textDocument/didSave").unwrap();
            assert!(save["params"]["textDocument"]["uri"].as_str().unwrap().starts_with("file://"));
        }

        #[test]
        fn deleting_the_buffer_closes_the_document() {
            let (_dir, mut ed, fake) = project("close");
            let id = handshake(&mut ed, &fake);

            ex(&mut ed, "bd");
            ed.settle();
            assert!(fake.last(id, "textDocument/didClose").is_some());
        }

        #[test]
        fn a_filetype_no_server_claims_is_a_reason_not_an_error() {
            let dir = ScratchDir::new("lsp-none").written("notes.md", "# hi\n");
            let mut ed = Editor::open(format!("{}/notes.md", dir.path())).unwrap();
            let fake = FakeSpawn::default();
            ed.set_lsp_spawner(fake.clone());
            ed.settle();

            assert!(fake.spawned.lock().unwrap().is_empty(), "nothing to spawn");
            ex(&mut ed, "lsp");
            assert!(ed.session.status.contains("no server for filetype"), "{}", ed.session.status);
        }

        #[test]
        fn a_crash_reaches_the_status_line_and_restart_spawns_a_fresh_instance() {
            let (_dir, mut ed, fake) = project("crash");
            let id = handshake(&mut ed, &fake);

            let inbox = fake.spawned.lock().unwrap()[0].1.clone();
            inbox.deliver(id, Inbound::Eof);
            ed.settle();
            assert!(ed.session.status.contains("rust-analyzer"), "{}", ed.session.status);
            ex(&mut ed, "lsp");
            assert!(ed.session.status.contains(":lsp restart"), "{}", ed.session.status);

            ex(&mut ed, "lsp restart");
            ed.settle();
            let spawned = fake.spawned.lock().unwrap();
            assert_eq!(spawned.len(), 2, "a fresh spawn");
            assert_ne!(spawned[1].0, id, "a restarted server is a new instance");
        }

        #[test]
        fn gd_jumps_where_the_answer_points_in_the_same_file() {
            let (_dir, mut ed, fake) = project("gd-here");
            let id = handshake(&mut ed, &fake);
            let uri = open_uri(&fake, id);

            ex(&mut ed, "definition");
            let sent = fake.last(id, "textDocument/definition").unwrap();
            assert_eq!(sent["params"]["position"], json!({ "line": 0, "character": 0 }));

            // "fn |main| ..." — the answer points at column 3.
            let answer = json!({ "uri": uri,
                "range": { "start": { "line": 0, "character": 3 },
                           "end": { "line": 0, "character": 7 } } });
            respond(&mut ed, &fake, id, "textDocument/definition", answer);
            assert_eq!(ed.cursor().unwrap().at, 3);
        }

        #[test]
        fn gd_opens_the_file_the_answer_names() {
            let (dir, mut ed, fake) = project("gd-cross");
            std::fs::write(dir.0.join("src/lib.rs"), "pub fn helper() {}\n").unwrap();
            let id = handshake(&mut ed, &fake);

            ex(&mut ed, "definition");
            let target = crate::lsp::pos::canonical(&dir.0.join("src/lib.rs")).unwrap();
            let answer = json!([{ "uri": crate::lsp::pos::uri_of(&target),
                "range": { "start": { "line": 0, "character": 7 },
                           "end": { "line": 0, "character": 13 } } }]);
            respond(&mut ed, &fake, id, "textDocument/definition", answer);

            let shown = ed.window().buffer().unwrap();
            let path = ed.entry(shown).buffer.path.clone().expect("a file-backed buffer");
            assert_eq!(path, target, "the other file is on screen");
            assert_eq!(ed.cursor().unwrap().at, 7);
        }

        #[test]
        fn a_definition_nobody_has_is_a_status_line_answer() {
            let (_dir, mut ed, fake) = project("gd-none");
            let id = handshake(&mut ed, &fake);

            ex(&mut ed, "definition");
            respond(&mut ed, &fake, id, "textDocument/definition", serde_json::Value::Null);
            assert_eq!(ed.session.status, "no definition found");
        }

        #[test]
        fn decl_and_impl_are_gd_under_their_own_methods() {
            let (_dir, mut ed, fake) = project("goto-kin");
            let id = handshake(&mut ed, &fake);
            let uri = open_uri(&fake, id);

            ex(&mut ed, "decl");
            let sent = fake.last(id, "textDocument/declaration").unwrap();
            assert_eq!(sent["params"]["position"], json!({ "line": 0, "character": 0 }));
            let answer = json!({ "uri": uri,
                "range": { "start": { "line": 0, "character": 3 },
                           "end": { "line": 0, "character": 7 } } });
            respond(&mut ed, &fake, id, "textDocument/declaration", answer);
            assert_eq!(ed.cursor().unwrap().at, 3);

            ex(&mut ed, "impl");
            respond(&mut ed, &fake, id, "textDocument/implementation", serde_json::Value::Null);
            assert_eq!(ed.session.status, "no implementation found");
        }

        #[test]
        fn a_kind_the_server_did_not_claim_is_refused_by_name() {
            let (_dir, mut ed, fake) = project("goto-uncl");
            ed.settle();
            let id = fake.spawned.lock().unwrap()[0].0;
            // The whole sync block, but only `definitionProvider` — the old
            // world, where `gd` works and the rest of the family does not.
            fake.grant(
                id,
                json!({ "positionEncoding": "utf-8",
                        "textDocumentSync": { "openClose": true, "change": 2, "save": true },
                        "definitionProvider": true }),
            );
            ed.settle();

            ex(&mut ed, "impl");
            assert_eq!(ed.session.status, "implementation: this server does not offer it");
            assert!(fake.last(id, "textDocument/implementation").is_none(), "nothing sent");

            ex(&mut ed, "decl");
            assert_eq!(ed.session.status, "declaration: this server does not offer it");
        }

        #[test]
        fn a_capability_the_server_lacks_refuses_before_sending() {
            let (_dir, mut ed, fake) = project("gd-nocap");
            // A handshake that offers nothing beyond sync.
            ed.settle();
            let id = fake.spawned.lock().unwrap()[0].0;
            fake.grant(id, json!({ "textDocumentSync": 2 }));
            ed.settle();

            ex(&mut ed, "definition");
            assert!(ed.session.status.contains("does not offer"), "{}", ed.session.status);
            assert_eq!(fake.last(id, "textDocument/definition"), None, "nothing was sent");
        }

        #[test]
        fn gr_builds_a_results_pane_titled_by_the_symbol() {
            let (_dir, mut ed, fake) = project("gr");
            let id = handshake(&mut ed, &fake);
            let uri = open_uri(&fake, id);

            // The cursor sits on `fn`, so `fn` is the symbol.
            ex(&mut ed, "references");
            let answer = json!([
                { "uri": uri, "range": { "start": { "line": 0, "character": 3 },
                                         "end": { "line": 0, "character": 7 } } },
                { "uri": uri, "range": { "start": { "line": 0, "character": 0 },
                                         "end": { "line": 0, "character": 2 } } },
            ]);
            respond(&mut ed, &fake, id, "textDocument/references", answer);

            let results = ed.window().results().expect("a results pane");
            assert_eq!(results.title, "references: fn");
            assert_eq!(results.matches().len(), 2);
            // Sorted by position, whatever order the server answered in.
            assert_eq!(results.matches()[0].col, 0);
            assert_eq!(results.matches()[1].col, 3);
            assert_eq!(results.matches()[0].text, "fn main() {");
            assert!(ed.session.status.contains("2 references in 1 file"), "{}", ed.session.status);
        }

        #[test]
        fn format_applies_the_edits_as_one_undo_step() {
            let (_dir, mut ed, fake) = project("fmt");
            let id = handshake(&mut ed, &fake);

            ex(&mut ed, "format");
            let sent = fake.last(id, "textDocument/formatting").unwrap();
            assert_eq!(sent["params"]["options"]["tabSize"], 4);
            assert_eq!(sent["params"]["options"]["insertSpaces"], true);

            // Two edits: replace `fn ` and append a comment line.
            let answer = json!([
                { "range": { "start": { "line": 0, "character": 0 },
                             "end": { "line": 0, "character": 3 } },
                  "newText": "pub fn " },
                { "range": { "start": { "line": 2, "character": 0 },
                             "end": { "line": 2, "character": 0 } },
                  "newText": "// end\n" },
            ]);
            respond(&mut ed, &fake, id, "textDocument/formatting", answer);
            assert_eq!(ed.buffer().unwrap().rope().to_string(), "pub fn main() {\n}\n// end\n");
            assert!(ed.session.status.contains("formatted"), "{}", ed.session.status);

            // The server heard about its own edits, or its copy is now wrong.
            let change = fake.last(id, "textDocument/didChange").unwrap();
            assert_eq!(change["params"]["textDocument"]["version"], 2);

            ed.apply(cmd(Action::Undo));
            ed.settle();
            assert_eq!(ed.buffer().unwrap().rope().to_string(), "fn main() {\n}\n", "one step");
        }

        #[test]
        fn a_format_computed_against_old_text_is_dropped() {
            let (_dir, mut ed, fake) = project("fmt-stale");
            let id = handshake(&mut ed, &fake);

            ex(&mut ed, "format");
            // The text moves on while the server thinks: version is now 2,
            // and the answer was computed against 1.
            ed.buffer_mut().unwrap().insert_str(Cursor::at(0), "x");
            ed.settle();

            let answer = json!([{ "range": { "start": { "line": 0, "character": 0 },
                                             "end": { "line": 1, "character": 0 } },
                                  "newText": "clobbered\n" }]);
            respond(&mut ed, &fake, id, "textDocument/formatting", answer);
            assert_eq!(ed.buffer().unwrap().rope().to_string(), "xfn main() {\n}\n", "untouched");
            assert!(ed.session.status.contains(":format"), "{}", ed.session.status);
        }

        #[test]
        fn diagnostic_jumps_walk_forward_backward_and_wrap() {
            let (_dir, mut ed, fake) = project("djump");
            let id = handshake(&mut ed, &fake);
            let uri = open_uri(&fake, id);
            let buffer = ed.window().buffer().unwrap();

            let range = |a: u32, b: u32| {
                json!({ "start": { "line": 0, "character": a },
                        "end": { "line": 0, "character": b } })
            };
            publish(
                &mut ed,
                &fake,
                id,
                json!({ "uri": uri, "version": 1, "diagnostics": [
                    { "range": range(3, 7), "severity": 1, "message": "first" },
                    { "range": range(10, 11), "severity": 2, "message": "second" },
                ]}),
            );
            assert_eq!(ed.diagnostics(buffer).len(), 2);

            ex(&mut ed, "dnext");
            assert_eq!(ed.cursor().unwrap().at, 3);
            assert!(ed.session.status.contains("[1/2] first"), "{}", ed.session.status);
            ex(&mut ed, "dnext");
            assert_eq!(ed.cursor().unwrap().at, 10);
            ex(&mut ed, "dnext");
            assert_eq!(ed.cursor().unwrap().at, 3, "wrapped");
            ex(&mut ed, "dprev");
            assert_eq!(ed.cursor().unwrap().at, 10, "wrapped the other way");
        }

        #[test]
        fn diagnostics_dress_the_text_and_the_gutter_until_told_not_to() {
            let (_dir, mut ed, fake) = project("ddress");
            let id = handshake(&mut ed, &fake);
            let uri = open_uri(&fake, id);

            publish(
                &mut ed,
                &fake,
                id,
                json!({ "uri": uri, "version": 1, "diagnostics": [
                    { "range": { "start": { "line": 0, "character": 3 },
                                 "end": { "line": 0, "character": 7 } },
                      "severity": 1, "message": "cannot find value" },
                ]}),
            );

            let error = ed.theme().ui.diag_error;
            let repaint = ed.decorations(ed.focus(), 0..2).into_iter().find_map(|d| match d {
                crate::decoration::Decoration::Repaint { range, style, .. } if style == error => {
                    Some(range)
                }
                _ => None,
            });
            assert_eq!(repaint, Some(3..7), "the range wears the severity");

            // The cursor is on row 0, so the message rides that row's end.
            let eol = ed.decorations(ed.focus(), 0..2).into_iter().find_map(|d| match d {
                crate::decoration::Decoration::Eol { row: 0, text, .. }
                    if text.contains("cannot find value") =>
                {
                    Some(text)
                }
                _ => None,
            });
            assert!(eol.is_some(), "the message at the line's end");

            let signs = ed.gutter_signs(ed.focus(), 0..2);
            assert_eq!(signs.len(), 1);
            assert_eq!((signs[0].0, signs[0].1), (0, '•'));
            assert!(!signs[0].2.underline, "the underline marks a range, not a sign");
            assert_eq!(signs[0].2.fg, error.fg, "but the severity's colour stays");

            // `:set diagnostics false` hides the lot without forgetting it.
            ex(&mut ed, "set diagnostics false");
            let drawn = ed.decorations(ed.focus(), 0..2).into_iter().any(|d| {
                matches!(d, crate::decoration::Decoration::Repaint { style, .. } if style == error)
            });
            assert!(!drawn, "hidden");
            assert!(ed.gutter_signs(ed.focus(), 0..2).is_empty());
            let buffer = ed.window().buffer().unwrap();
            assert_eq!(ed.diagnostics(buffer).len(), 1, "but not forgotten");
        }

        #[test]
        fn diags_lists_the_stored_diagnostics_worst_first_in_a_results_pane() {
            let (_dir, mut ed, fake) = project("diags");
            let id = handshake(&mut ed, &fake);
            let uri = open_uri(&fake, id);

            publish(
                &mut ed,
                &fake,
                id,
                json!({
                    "uri": uri, "version": 1,
                    "diagnostics": [
                        { "range": { "start": { "line": 0, "character": 3 },
                                     "end": { "line": 0, "character": 7 } },
                          "severity": 2, "message": "dubious\nsecond line" },
                        { "range": { "start": { "line": 1, "character": 0 },
                                     "end": { "line": 1, "character": 1 } },
                          "severity": 1, "message": "broken" },
                    ]
                }),
            );

            ex(&mut ed, "diags");

            let results = ed.window().results().expect("a results pane");
            assert_eq!(results.title, "diagnostics");
            assert_eq!(results.matches().len(), 2);
            // The error on line 2 outranks the warning on line 1.
            assert_eq!(results.matches()[0].line, 2);
            assert_eq!(results.matches()[0].text, "}  ▸ E: broken");
            // The message's first line only, after the text it is about.
            assert_eq!(results.matches()[1].line, 1);
            assert_eq!(results.matches()[1].text, "fn main() {  ▸ W: dubious");
            assert_eq!((results.matches()[1].col, results.matches()[1].len), (3, 4));
            assert_eq!(ed.session.status, "2 diagnostics in 1 file");
        }

        #[test]
        fn the_diagnostic_wins_the_gutter_cell_over_the_git_sign() {
            let (_dir, mut ed, fake) = project("cell");
            let id = handshake(&mut ed, &fake);
            let uri = open_uri(&fake, id);
            // Row 0 differs from the index — a git change sign.
            ed.set_git_baseline(|_| Some("x\n}\n".into()));
            let signs = ed.gutter_signs(ed.focus(), 0..2);
            assert_eq!((signs.len(), signs[0].1), (1, '▎'));

            publish(
                &mut ed,
                &fake,
                id,
                json!({
                    "uri": uri, "version": 1,
                    "diagnostics": [{ "range": { "start": { "line": 0, "character": 0 },
                                                 "end": { "line": 0, "character": 2 } },
                                      "severity": 1, "message": "nope" }]
                }),
            );
            let signs = ed.gutter_signs(ed.focus(), 0..2);
            assert_eq!((signs.len(), signs[0].1), (1, '•'), "one cell, the diagnostic over it");

            // Hiding the diagnostics uncovers the git sign under them.
            ex(&mut ed, "set diagnostics false");
            let signs = ed.gutter_signs(ed.focus(), 0..2);
            assert_eq!((signs.len(), signs[0].1), (1, '▎'));
        }

        #[test]
        fn hover_floats_at_its_anchor_and_the_next_command_clears_it() {
            let (_dir, mut ed, fake) = project("hover");
            let id = handshake(&mut ed, &fake);

            ex(&mut ed, "hover");
            let sent = fake.last(id, "textDocument/hover").unwrap();
            assert_eq!(sent["params"]["position"], json!({ "line": 0, "character": 0 }));

            let answer = json!({ "contents": { "kind": "markdown",
                "value": "```rust\nfn main()\n```\n---\nThe entry point." } });
            respond(&mut ed, &fake, id, "textDocument/hover", answer);

            let hover = ed.session.hover.as_ref().expect("a float");
            assert_eq!(hover.anchor, 0);
            assert_eq!(hover.language, Some("rust"));
            assert_eq!(
                hover.lines,
                vec![
                    HoverLine::Code("fn main()".into()),
                    HoverLine::Rule,
                    HoverLine::Text("The entry point.".into()),
                ]
            );

            // The flash's rule: doing anything else dismisses it.
            ed.apply(cmd(Action::Move(Motion::Right)));
            assert!(ed.session.hover.is_none());
        }

        #[test]
        fn a_hover_with_nothing_to_say_says_so_on_the_status_line() {
            let (_dir, mut ed, fake) = project("hover-none");
            let id = handshake(&mut ed, &fake);
            ex(&mut ed, "hover");
            respond(&mut ed, &fake, id, "textDocument/hover", serde_json::Value::Null);
            assert!(ed.session.hover.is_none());
            assert_eq!(ed.session.status, "no hover info here");
        }

        /// Typing opens the menu, narrowing keeps it honest, Tab accepts —
        /// the whole loop, driven by ordinary commands.
        #[test]
        fn typing_summons_the_menu_and_tab_accepts_into_the_buffer() {
            let (_dir, mut ed, fake) = project("menu");
            let id = handshake(&mut ed, &fake);

            ed.apply(cmd(Action::EnterInsert));
            for c in "po".chars() {
                ed.apply(cmd(Action::InsertChar(c)));
                ed.settle();
            }
            // Each closed-menu word char asked; only the newest ask counts.
            let sent = fake.last(id, "textDocument/completion").unwrap();
            assert_eq!(sent["params"]["position"], json!({ "line": 0, "character": 2 }));
            assert_eq!(sent["params"]["context"]["triggerKind"], 1);

            let answer = json!([
                { "label": "pos", "sortText": "b" },
                { "label": "position_of", "sortText": "a" },
                { "label": "unrelated", "sortText": "c" },
            ]);
            respond(&mut ed, &fake, id, "textDocument/completion", answer);

            let menu = ed.session.completion.as_ref().expect("open");
            let labels: Vec<&str> = menu.matches().map(|i| i.label.as_str()).collect();
            assert_eq!(labels, ["position_of", "pos"], "prefix bucket, by sortText");
            assert_eq!(menu.replace, 0..2, "the word being typed");

            // Typing narrows without asking again.
            ed.apply(cmd(Action::InsertChar('s')));
            ed.settle();
            let menu = ed.session.completion.as_ref().unwrap();
            assert_eq!(menu.replace, 0..3);
            assert_eq!(menu.matches().count(), 2, "pos and position_of both match pos");

            // Ctrl-N moves; Tab accepts the selection into the buffer.
            ed.apply(cmd(Action::CompleteNext));
            ed.apply(cmd(Action::InsertIndent { right: true }));
            assert!(ed.session.completion.is_none());
            assert_eq!(ed.session.mode, Mode::Insert, "still typing");
            assert!(
                ed.buffer().unwrap().rope().to_string().starts_with("posfn"),
                "the second offer replaced the word: {}",
                ed.buffer().unwrap().rope()
            );
            assert_eq!(ed.cursor().unwrap().at, 3, "after what was inserted");
        }

        #[test]
        fn esc_closes_the_menu_and_stays_in_insert() {
            let (_dir, mut ed, fake) = project("menu-esc");
            let id = handshake(&mut ed, &fake);

            ed.apply(cmd(Action::EnterInsert));
            ed.apply(cmd(Action::InsertChar('p')));
            ed.settle();
            respond(&mut ed, &fake, id, "textDocument/completion", json!([{ "label": "pos" }]));
            assert!(ed.session.completion.is_some());

            ed.apply(cmd(Action::EnterNormal));
            assert!(ed.session.completion.is_none());
            assert_eq!(ed.session.mode, Mode::Insert, "the menu was dismissed, not the typing");

            ed.apply(cmd(Action::EnterNormal));
            assert_eq!(ed.session.mode, Mode::Normal, "and the second Esc leaves");
        }

        #[test]
        fn a_stale_answer_is_dropped_and_a_trigger_char_carries_its_context() {
            let (_dir, mut ed, fake) = project("menu-stale");
            let id = handshake(&mut ed, &fake);

            ed.apply(cmd(Action::EnterInsert));
            ed.apply(cmd(Action::InsertChar('p')));
            ed.settle();
            let first = fake.last(id, "textDocument/completion").unwrap()["id"].as_i64().unwrap();
            ed.apply(cmd(Action::InsertChar('o')));
            ed.settle();

            // The first ask's answer arrives after the second ask went out.
            let inbox = fake.spawned.lock().unwrap()[0].1.clone();
            inbox.deliver(
                id,
                Inbound::Response { id: first, result: Ok(json!([{ "label": "stale" }])) },
            );
            ed.settle();
            assert!(ed.session.completion.is_none(), "yesterday's answer");

            // `.` is a server trigger character and says so.
            ed.apply(cmd(Action::InsertChar('.')));
            ed.settle();
            let sent = fake.last(id, "textDocument/completion").unwrap();
            assert_eq!(sent["params"]["context"]["triggerKind"], 2);
            assert_eq!(sent["params"]["context"]["triggerCharacter"], ".");
        }

        #[test]
        fn accepting_applies_the_auto_import_and_lands_the_cursor() {
            let (_dir, mut ed, fake) = project("menu-import");
            let id = handshake(&mut ed, &fake);

            ed.apply(cmd(Action::EnterInsert));
            ed.apply(cmd(Action::InsertChar('p')));
            ed.settle();
            let answer = json!([{
                "label": "pos", "insertText": "pos()",
                "additionalTextEdits": [{
                    "range": { "start": { "line": 0, "character": 0 },
                               "end": { "line": 0, "character": 0 } },
                    "newText": "use x::pos;\n"
                }]
            }]);
            respond(&mut ed, &fake, id, "textDocument/completion", answer);

            ed.apply(cmd(Action::InsertIndent { right: true }));
            ed.settle();
            let text = ed.buffer().unwrap().rope().to_string();
            assert!(text.starts_with("use x::pos;\npos()fn"), "{text}");
            assert_eq!(ed.cursor().unwrap().at, 17, "after the insert, shifted by the import");
        }

        #[test]
        fn a_manual_summons_reports_an_empty_answer_and_a_missing_capability() {
            let (_dir, mut ed, fake) = project("menu-manual");
            let id = handshake(&mut ed, &fake);

            ed.apply(cmd(Action::EnterInsert));
            ed.apply(cmd(Action::CompleteNext));
            ed.settle();
            respond(&mut ed, &fake, id, "textDocument/completion", json!([]));
            assert_eq!(ed.session.status, "no completions here");

            // Snippets collapse to their text on the way into the menu.
            ed.apply(cmd(Action::CompleteNext));
            ed.settle();
            let answer = json!([{ "label": "println!",
                "insertText": "println!(\"$1\")$0", "insertTextFormat": 2 }]);
            respond(&mut ed, &fake, id, "textDocument/completion", answer);
            let menu = ed.session.completion.as_ref().expect("open");
            assert_eq!(menu.selected_item().unwrap().insert, "println!(\"\")");
        }

        #[test]
        fn an_open_paren_floats_the_signature_and_the_call_ending_closes_it() {
            let (_dir, mut ed, fake) = project("sig");
            let id = handshake(&mut ed, &fake);

            ed.apply(cmd(Action::EnterInsert));
            ed.apply(cmd(Action::InsertChar('(')));
            ed.settle();
            let sent = fake.last(id, "textDocument/signatureHelp").unwrap();
            assert_eq!(sent["params"]["context"]["triggerCharacter"], "(");

            let answer = json!({
                "signatures": [{ "label": "fn main(argc: i32)",
                                 "parameters": [{ "label": "argc: i32" }] }],
            });
            respond(&mut ed, &fake, id, "textDocument/signatureHelp", answer);
            let sig = ed.session.signature.as_ref().expect("a float");
            assert_eq!(sig.data.label, "fn main(argc: i32)");
            assert_eq!(sig.data.active, Some(8..17));

            // While it is up, typing follows the cursor by re-asking…
            ed.apply(cmd(Action::InsertChar('x')));
            ed.settle();
            let sent = fake.last(id, "textDocument/signatureHelp").unwrap();
            assert_eq!(sent["params"]["context"]["isRetrigger"], true);

            // …and the server answering null is the call ending.
            respond(&mut ed, &fake, id, "textDocument/signatureHelp", serde_json::Value::Null);
            assert!(ed.session.signature.is_none(), "closed by the server's silence");
        }

        #[test]
        fn leaving_insert_mode_takes_the_signature_with_it() {
            let (_dir, mut ed, fake) = project("sig-esc");
            let id = handshake(&mut ed, &fake);

            ed.apply(cmd(Action::EnterInsert));
            ed.apply(cmd(Action::InsertChar('(')));
            ed.settle();
            let answer = json!({ "signatures": [{ "label": "f()" }] });
            respond(&mut ed, &fake, id, "textDocument/signatureHelp", answer);
            assert!(ed.session.signature.is_some());

            ed.apply(cmd(Action::EnterNormal));
            assert!(ed.session.signature.is_none());

            // And a stale answer cannot resurrect it: the seq moved on.
            ed.apply(cmd(Action::EnterInsert));
            ed.apply(cmd(Action::InsertChar('(')));
            ed.settle();
            ed.apply(cmd(Action::InsertChar(',')));
            ed.settle();
            let old = fake
                .sent
                .lock()
                .unwrap()
                .iter()
                .filter(|(sid, m)| *sid == id && m["method"] == "textDocument/signatureHelp")
                .nth_back(1)
                .map(|(_, m)| m["id"].as_i64().unwrap())
                .unwrap();
            let inbox = fake.spawned.lock().unwrap()[0].1.clone();
            inbox.deliver(
                id,
                Inbound::Response {
                    id: old,
                    result: Ok(json!({ "signatures": [{ "label": "stale()" }] })),
                },
            );
            ed.settle();
            assert!(ed.session.signature.is_none(), "yesterday's answer");
        }

        #[test]
        fn lsp_stop_stands_the_server_down_and_keeps_it_down() {
            let (_dir, mut ed, fake) = project("stop");
            let id = handshake(&mut ed, &fake);

            ex(&mut ed, "lsp stop");
            assert!(fake.killed.lock().unwrap().contains(&id));
            ed.settle();
            ed.settle();
            assert_eq!(fake.spawned.lock().unwrap().len(), 1, "no quiet resurrection");
            ex(&mut ed, "lsp");
            assert!(ed.session.status.contains("stopped"), "{}", ed.session.status);
        }
    }
    /// A real PNG in a scratch directory, for the image-pane tests.
    struct PngDir(std::path::PathBuf);

    impl PngDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("bi-ed-img-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// A 50×40 PNG named `photo.png`.
        fn png(&self) -> std::path::PathBuf {
            let path = self.0.join("photo.png");
            image::RgbaImage::from_pixel(50, 40, image::Rgba([9, 9, 9, 255])).save(&path).unwrap();
            path
        }
    }

    impl Drop for PngDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_image_path_opens_as_an_image_not_a_buffer() {
        let d = PngDir::new("open");
        let mut ed = Editor::empty();
        let buffers = ed.buffer_ids().len();

        ed.run_ex(&format!("e {}", d.png().display()));

        assert_eq!(ed.content_kind(), ContentKind::Image);
        assert!(ed.session.status.contains("50×40"), "{}", ed.session.status);
        assert_eq!(ed.buffer_ids().len(), buffers, "the buffer list gained nothing");
    }

    #[test]
    fn opening_bi_on_an_image_shows_the_image() {
        let d = PngDir::new("startup");
        let ed = Editor::open(d.png()).unwrap();
        assert_eq!(ed.content_kind(), ContentKind::Image);
    }

    #[test]
    fn a_corrupt_image_falls_back_to_text_and_says_why() {
        let d = PngDir::new("corrupt");
        let path = d.0.join("broken.png");
        std::fs::write(&path, "not a png").unwrap();
        let mut ed = Editor::empty();

        ed.run_ex(&format!("e {}", path.display()));

        assert_eq!(ed.content_kind(), ContentKind::Text, "the bytes as they are");
        assert!(ed.session.status.contains("opened as text"), "{}", ed.session.status);
    }

    #[test]
    fn motions_scroll_the_image_and_counts_multiply() {
        let d = PngDir::new("scroll");
        let mut ed = Editor::empty();
        ed.run_ex(&format!("e {}", d.png().display()));
        let focus = ed.focus();
        ed.image_pane_mut(focus).unwrap().set_viewport(20, 20, 5);

        ed.apply(Command { count: 2, action: Action::Move(Motion::Down) });
        ed.apply(cmd(Action::Move(Motion::Right)));
        assert_eq!(ed.window().img().unwrap().scroll(), (5, 10));

        ed.apply(cmd(Action::Move(Motion::LastLine)));
        ed.apply(cmd(Action::Move(Motion::LineEnd)));
        assert_eq!(ed.window().img().unwrap().scroll(), (30, 20), "the far corner");

        ed.apply(cmd(Action::Move(Motion::FirstLine)));
        ed.apply(cmd(Action::Move(Motion::LineStart)));
        assert_eq!(ed.window().img().unwrap().scroll(), (0, 0));
    }

    /// `i`, `v`, `/`, `s` — a search line over a picture is a promise the
    /// pane cannot keep, so none of them get to move the mode.
    #[test]
    fn modes_do_not_exist_in_an_image_window() {
        let d = PngDir::new("modes");
        let mut ed = Editor::empty();
        ed.run_ex(&format!("e {}", d.png().display()));

        ed.apply(cmd(Action::EnterInsert));
        ed.apply(cmd(Action::EnterVisual(Shape::Chars)));
        ed.apply(cmd(Action::EnterFind));
        ed.apply(cmd(Action::EnterSearch { forward: true, operator: None, count: 1 }));
        ed.apply(cmd(Action::ShowScopes));

        assert!(matches!(ed.session.mode, Mode::Normal), "{:?}", ed.session.mode);
    }

    #[test]
    fn ctrl_caret_swaps_an_image_out_and_back_with_its_crop() {
        let d = PngDir::new("alt");
        let mut ed = Editor::empty();
        ed.run_ex(&format!("e {}", d.png().display()));
        let focus = ed.focus();
        ed.image_pane_mut(focus).unwrap().set_viewport(20, 20, 5);
        ed.apply(cmd(Action::Move(Motion::Down)));
        assert_eq!(ed.window().img().unwrap().scroll(), (0, 5));

        ed.apply(cmd(Action::Buffer(BufferCmd::Alternate)));
        assert_eq!(ed.content_kind(), ContentKind::Text, "back to what it displaced");

        ed.apply(cmd(Action::Buffer(BufferCmd::Alternate)));
        assert_eq!(ed.content_kind(), ContentKind::Image);
        assert_eq!(ed.window().img().unwrap().scroll(), (0, 5), "the crop survived");
    }

    /// `:bd` means "take this away", and refusing it on a technicality is
    /// the detached feeling the design exists to avoid.
    #[test]
    fn bd_dismisses_an_image_and_brings_the_alternate_back() {
        let d = PngDir::new("bd");
        let mut ed = Editor::empty();
        ed.run_ex(&format!("e {}", d.png().display()));
        assert_eq!(ed.content_kind(), ContentKind::Image);

        ed.run_ex("bd");

        assert_eq!(ed.content_kind(), ContentKind::Text, "the alternate came back");
        ed.apply(cmd(Action::Buffer(BufferCmd::Alternate)));
        assert_eq!(ed.content_kind(), ContentKind::Text, "deleted, not parked");
    }

    /// The startup image has no alternate — `bi photo.png` was the session's
    /// first content — so `:bd` shows the most recent buffer instead.
    #[test]
    fn bd_on_an_image_with_no_alternate_shows_a_buffer() {
        let d = PngDir::new("bd-mru");
        let mut ed = Editor::open(d.png()).unwrap();
        assert_eq!(ed.content_kind(), ContentKind::Image);

        ed.run_ex("bd");

        assert_eq!(ed.content_kind(), ContentKind::Text);
    }

    /// A bare `:vs` clones the window, crop and all — and from there the two
    /// panes are two views, which is the reason to open the second one.
    #[test]
    fn a_bare_split_on_an_image_gives_two_independent_crops() {
        let d = PngDir::new("split");
        let mut ed = Editor::empty();
        sized(&mut ed);
        ed.run_ex(&format!("e {}", d.png().display()));

        ed.run_ex("vs");

        let ids = ed.window_ids();
        assert_eq!(ids.len(), 2);
        let imgs: Vec<u64> = ids
            .iter()
            .filter_map(|&id| ed.window_of(id).and_then(Window::img).map(|img| img.id))
            .collect();
        assert_eq!(imgs.len(), 2, "both panes show the image");
        assert_eq!(imgs[0], imgs[1], "the same pixels — one upload, two placements");

        let focus = ed.focus();
        ed.image_pane_mut(focus).unwrap().set_viewport(20, 20, 5);
        ed.apply(cmd(Action::Move(Motion::Down)));

        let scrolls: Vec<(u32, u32)> = ids
            .iter()
            .filter_map(|&id| ed.window_of(id).and_then(Window::img).map(|img| img.scroll()))
            .collect();
        assert!(scrolls.contains(&(0, 5)), "the focused crop moved: {scrolls:?}");
        assert!(scrolls.contains(&(0, 0)), "the other did not: {scrolls:?}");
    }

    /// The command line still works over an image — `:` is how it closes.
    #[test]
    fn the_ex_line_still_opens_over_an_image() {
        let d = PngDir::new("ex");
        let mut ed = Editor::empty();
        ed.run_ex(&format!("e {}", d.png().display()));

        ed.apply(cmd(Action::EnterCommandMode));

        assert!(matches!(ed.session.mode, Mode::Command(_)));
    }
}
