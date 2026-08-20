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
use crate::config::{Config, ConfigSource, Diagnostic, OptionPatch, OptionValue, Options};
use crate::history::Cursors;
use crate::motion::{Motion, Operator, Target, TextObject};
use crate::picker::{Item, Picker, PickerKind, REGISTER_MIN_LEN};
use crate::range::{Address, LineRange, Where};
use crate::registers::{Entry, EntryKind, Registers, Sink};
use crate::selection::{Selection, Selections};
use crate::syntax::Syntax;
use crate::theme::Theme;
use crate::tree::{ClipMode, Clipboard, Kind, Mark, Tree, copy_into, move_into};
use crate::window::{
    Chrome, Content, ContentKind, Dir, Layout, Place, Rect, Side, Text, Window, WindowId,
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
            Mode::Visual(VisualKind::Char) => "VISUAL",
            Mode::Visual(VisualKind::Line) => "V-LINE",
            Mode::Visual(VisualKind::Block) => "V-BLOCK",
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
    /// Where an operator's text goes.
    ///
    /// The one place a `Sink` is spent, so a new register is a new arm here
    /// rather than a fourth copy of this decision at a third call site.
    fn capture(&mut self, entry: Entry, sink: Sink) {
        match sink {
            Sink::Ring => self.registers.push(entry),
            // Nothing ever reaches the black hole, which is the point of it.
            Sink::BlackHole => {}
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
        let kind = if text.ends_with('\n') { EntryKind::Linewise } else { EntryKind::Charwise };
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
    /// than one consumer — tree-sitter now, LSP `didChange` later — and
    /// whoever drains it destroys it for the others. Putting the tree on
    /// `Buffer` would move the drain inside the buffer and break that.
    syntax: Option<Syntax>,
    /// What kind of file this is — `rust`, `make` — or `None` when nothing
    /// claims the name. The key the option layers and the grammar are both
    /// chosen by; see `crate::syntax::filetype`.
    filetype: Option<&'static str>,
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
}

impl BufferEntry {
    /// Options are left at the defaults here and resolved by
    /// `Editor::resolve_options`, which is the only thing that can see the
    /// layers under them.
    fn new(id: BufferId, buffer: Buffer) -> Self {
        Self {
            id,
            filetype: filetype_of(&buffer),
            syntax: syntax_for(&buffer),
            buffer,
            options: Options::default(),
            last: Vec::new(),
        }
    }
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
    /// `:m +3`, `:2,5m 0`, `:m $`.
    ///
    /// `lines` is what the range in front of the command said, and `None` is
    /// what it says when nobody wrote one — where `:m`'s own default lives,
    /// which is the selection. See `docs/specs/ranges.md`.
    Move {
        lines: Option<LineRange>,
        to: Address,
    },
    /// `:case snake` — respell what is selected, or the word under the cursor.
    Case(crate::case::Style),
    /// `:alt` — the other file: the test beside the implementation, the
    /// header beside the source.
    Alternate,
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
    let (range, rest) = crate::range::parse(arg.trim());
    let range = range?;
    // One address, not a span: `:m 2,5` names two lines to land after.
    (rest.is_empty() && range.first == range.last).then_some(range.first)
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
    let (range, line) = crate::range::parse(line);
    if line.is_empty() {
        // A range and no command goes to its last line: `:42`, `:$`, `:%`.
        return Some(match range {
            Some(range) => ExLine::Goto(range.last),
            None => return None,
        });
    }

    let (cmd, arg) = match line.split_once(char::is_whitespace) {
        Some((c, a)) => (c, a.trim()),
        None => (line, ""),
    };
    // Nothing after a space, so the argument may be stuck to the name.
    let (cmd, arg) = match arg.is_empty() {
        true => split_glued_move(cmd).unwrap_or((cmd, arg)),
        false => (cmd, arg),
    };
    let force = cmd.ends_with('!');
    let name = cmd.trim_end_matches('!');
    let split = |dir| {
        ExLine::Window(WindowCmd::Split { dir, path: (!arg.is_empty()).then(|| arg.to_string()) })
    };

    // A range handed to a command that has no use for one is an error rather
    // than a range quietly dropped: vim writes part of a file for `:1,5w` and
    // bi does not, and a command that ignores half of what you typed is the
    // worse of the two ways to not support something.
    if range.is_some() && !matches!(name, "m" | "move") {
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
            Some(to) => ExLine::Move { lines: range, to },
            None => ExLine::Error("move where? `:m +3`, `:m -2`, `:m 0`, `:m $`".into()),
        },
        "alt" | "alternate" => ExLine::Alternate,
        "case" => match crate::case::Style::parse(arg) {
            Some(style) => ExLine::Case(style),
            None => {
                ExLine::Error(format!("case what? one of {}", crate::case::Style::NAMES.join(", ")))
            }
        },
        "wq" | "x" => ExLine::WriteQuit(arg.into()),
        "reload" => ExLine::ReloadConfig,
        // A bare line number never reaches here: it is a range with no
        // command, and was handled before the table.
        _ => ExLine::Unknown(name.into()),
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

/// Picks a grammar from the file's name. An unknown one yields `None`, which
/// renders as plain text.
///
/// The whole name rather than the extension, so a grammar can claim
/// `CMakeLists.txt`. Which key wins is `syntax.rs`'s business, not this
/// function's.
fn syntax_for(buffer: &Buffer) -> Option<Syntax> {
    let name = buffer
        .path
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_string();
    Syntax::new(&name, buffer.rope())
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
            let tree = Tree::new(path)?;
            // The directory bi was pointed at is the session's root from here
            // on, whatever displaces the tree later.
            editor.session.tree_root = Some(tree.root().to_path_buf());
            // Assigned rather than shown: there was nothing here before, so
            // there is no alternate to remember.
            editor.window_mut().content = Content::Tree(tree);
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
        let text = match source.config() {
            Ok(Some(text)) => text,
            // No file is not an error, but it is still a fact about the
            // world that can change between calls — a file present at
            // startup can be gone by `:reload`. Applying the defaults here,
            // rather than leaving whatever was last applied, is what keeps
            // this path agreeing with startup on an editor that has never
            // seen a file at all.
            Ok(None) => return Ok(self.apply_config(Config::default(), Some(source))),
            Err(e) => return Err(Diagnostic { line: 1, message: e.to_string() }),
        };

        match crate::config::parse(&text, Config::default()) {
            Ok((config, mut problems)) => {
                // Theme problems join config problems: both came out of
                // loading, and a frontend that reports one should report the
                // other without learning there were two files.
                problems.extend(self.apply_config(config, Some(source)));
                Ok(problems)
            }
            // Unsalvageable: the running config stays exactly as it was.
            Err(problem) => Err(problem),
        }
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
        problems
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
            area: Rect::default(),
            chrome: Chrome::default(),
            session: Session::default(),
            config: Config::default(),
            theme: Theme::default(),
            remote: false,
            config_source: None,
            config_epoch: 0,
        };
        editor.resolve_options();
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
                        self.session.mode = Mode::Visual(VisualKind::Char);
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
        let Some(Pane::Text { buffer, options, .. }) = self.pane(window) else { return out };
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
        if options.indent_guides {
            indent_guides(buffer, options, &self.theme, rows.clone(), &mut out);
        }
        if options.todo_comments {
            todo_comments(buffer, &self.theme, rows.clone(), &mut out);
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
            options: &mut entry.options,
            filetype: &mut entry.filetype,
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
        self.buffers.push(BufferEntry::new(id, buffer));
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

    /// `gf` in a tree — the picker over the rows it is showing.
    ///
    /// The rows rather than the filesystem, which is the whole difference from
    /// `Ctrl-P`: this moves the selection inside the pane, so it can only
    /// offer what the pane has on it. Each row is named by its path below the
    /// root, so a query can say which `mod.rs`. See `docs/specs/tree.md`.
    fn open_tree_picker(&mut self) {
        let Some(tree) = self.window().tree() else { return };
        let root = tree.root().to_path_buf();
        // From row 1: row 0 is the root itself, which has no path below the
        // root to be named by and is where `gg` already goes.
        let items: Vec<Item> = tree.rows()[1..]
            .iter()
            .map(|row| Item {
                text: row.path.strip_prefix(&root).unwrap_or(&row.path).to_string_lossy().into(),
                badge: (row.kind == Kind::Dir).then_some('/'),
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
    /// `chosen + 1` because the list left the root row out. Nothing can have
    /// moved in between — the picker held every key while it was open.
    fn select_tree_row(&mut self, chosen: usize) {
        let height = self.window().height;
        let Some(tree) = self.window_mut().tree_mut() else { return };
        tree.select(chosen + 1);
        tree.scroll_to_selected(height);
    }

    fn open_buffer_picker(&mut self) {
        let ids = self.mru_ids();
        let items = ids
            .iter()
            .map(|&id| Item { text: self.name_of(id), badge: self.is_modified(id).then_some('+') })
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
            Mode::Command(line) => line.clone(),
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

                let Some(new) = self.split_focus(dir) else { return };
                match content {
                    // Through `show`, so the new window records where the
                    // duplicated one was before it moves off that buffer.
                    Some(Content::Text(text)) => self.show(new, text.buffer),
                    Some(tree) => self.window_mut().show(tree),
                    None => {}
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
                let tree = match Tree::new(&root) {
                    Ok(tree) => tree,
                    Err(e) => {
                        self.session.status = format!("{e:#}");
                        return;
                    }
                };
                self.session.tree_root = Some(tree.root().to_path_buf());

                let new = self.fresh_window_id();
                // A file tree belongs on the left, whichever side a plain
                // split opens on.
                if !self.layout.split(focus, new, Dir::Vertical, Place::Before, area, &chrome) {
                    self.next_window -= 1;
                    self.session.status = "not enough room to split".into();
                    return;
                }
                self.windows.push(Window::showing(new, Content::Tree(tree)));
                self.set_focus(new);
                self.reveal_in_tree(path.as_deref());

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
                // Before running it, so a command that failed is the one you
                // can recall and fix — which is most of what a history is for.
                // Only what was typed here: an `Ex` action is a keybinding or
                // an internal caller, and a history of lines you never typed is
                // noise in the list that exists to give your own back.
                self.session.cmd_history.push(&line);
                self.run_ex(&line);
            }
            Action::Ex { line, run } => {
                let line = line.clone();
                match run {
                    true => self.run_ex(&line),
                    false => self.session.mode = Mode::Command(line),
                }
            }

            // Here rather than in the view, beside the `:` line it is opened
            // from: both have to work in a window holding a tree, where there
            // is no rope to run anything against.
            Action::OpenPicker(PickerKind::History) => self.open_history_picker(),
            Action::OpenPicker(PickerKind::File) => self.open_file_picker(),
            // This one is a tree pane by definition.
            Action::OpenPicker(PickerKind::TreeRow) => self.open_tree_picker(),

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
            PickerKind::TreeRow => self.select_tree_row(chosen),
            PickerKind::Register { before } => {
                self.in_view(|view| view.paste_pick(chosen, before));
            }
            // Put back on the `:` line, unrun. Editing it is the point: the
            // line you reach for a history for is the one with a word wrong.
            PickerKind::History => {
                let line = picker.items()[chosen].text.clone();
                self.session.mode = Mode::Command(line);
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
        // you remember which command a directory wants. A directory re-roots
        // the pane you are in, tree or not — that is what naming one means.
        if std::path::Path::new(path).is_dir() {
            return self.show_tree(path);
        }
        let target = self.open_target();
        match self.open_path(path) {
            Ok(id) => {
                self.show(target, id);
                self.set_focus(target);
                self.session.status = format!("\"{}\" loaded", self.name_of(id));
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
        // Naming a directory is one of the three ways to say where the session
        // is scoped — `+` and `-` are the others.
        self.session.tree_root = Some(tree.root().to_path_buf());
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
                self.session.mode = Mode::Command(format!("paste-as {}", target.display()));
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
            ExLine::Highlight(on) => self.session.options.hlsearch = on,
            ExLine::Set(arg) => self.set_option(&arg),
            ExLine::Create(path) => self.create_path(&path),
            ExLine::Rename { from, to } => self.rename_path(&from, &to),
            ExLine::Delete { path, force } => self.delete_path(&path, force),
            ExLine::Paste(dir) => match dir {
                Some(dir) => self.paste_into(std::path::Path::new(&dir)),
                None => self.paste_into_selected(),
            },
            ExLine::PasteAs(path) => self.run_paste(Some(path.into())),
            ExLine::Move { lines, to } => {
                self.in_view(|view| view.move_to(lines, to));
            }
            ExLine::Case(style) => {
                self.in_view(|view| view.recase(style));
            }
            ExLine::Alternate => self.open_alternate(),
            ExLine::Unknown(name) => self.session.status = format!("not a command: {name}"),
            ExLine::Error(message) => self.session.status = message,
            ExLine::ReloadConfig => self.reload_config(),

            // The rest need the rope, and so need a view.
            ExLine::Write(path) => {
                self.in_view(|view| view.write(&path));
            }
            ExLine::Revert { force } => {
                self.in_view(|view| view.edit(force));
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

    /// `:set <option> <value>`, or `:set <option>=<value>` — vim's spelling,
    /// which the fingers type without asking. Bare `:set <option>` reports.
    ///
    /// The names and their meanings live in [`Options`], not here, so `:set`
    /// and `config.toml` cannot disagree about what an option is or what it
    /// accepts.
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
        // alone would report success and change nothing on screen.
        if self.session.options.active_theme(self.remote) != was {
            let source = self.config_source.take();
            let problems = self.resolve_theme(source.as_deref());
            self.config_source = source;
            self.session.status = match problems.first() {
                Some(problem) => problem.message.clone(),
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
        if sink != Sink::BlackHole {
            let rows: Vec<String> =
                spans.iter().map(|&(start, end)| self.buffer.slice(start, end)).collect();
            // One entry, not one per row: what was taken is a rectangle, and
            // pasting it back has to know that.
            self.session.capture(Entry { text: rows.join("\n"), kind: EntryKind::Blockwise }, sink);
        }

        if op == Operator::Yank {
            for &(start, end) in &spans {
                if end > start {
                    self.flash(start..end);
                }
            }
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
        if self.session.mode.visual() == Some(VisualKind::Block) {
            self.paste_over_block(entry, capture, count);
            return;
        }
        let linewise = self.session.mode.visual() == Some(VisualKind::Line);
        self.for_each_selection(|ed, sel| {
            let len = ed.buffer.rope().len_chars();
            let (start, end) = if linewise {
                let (lo, hi) = sel.range();
                ed.buffer.line_range(lo, hi, true)
            } else {
                // Charwise visual includes the character under the head.
                sel.inclusive_range(len)
            };
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
        let spans = self.block_spans();
        if capture {
            let rows: Vec<String> =
                spans.iter().map(|&(start, end)| self.buffer.slice(start, end)).collect();
            // One entry, not one per row: what was taken is a rectangle, and
            // pasting it back has to know that.
            self.session
                .registers
                .push(Entry { text: rows.join("\n"), kind: EntryKind::Blockwise });
        }

        let top_left = spans.first().map(|&(start, _)| start).unwrap_or(0);
        let bottom = spans.last().map(|&(start, _)| start).unwrap_or(0);
        let last_row = self.buffer.row_at(Cursor::at(bottom));

        let landed = match entry.kind {
            EntryKind::Charwise => {
                self.replace_spans(&spans, entry, count);
                self.buffer.clamped(Cursor::at(top_left), false)
            }
            EntryKind::Linewise => {
                self.cut_spans(&spans);
                let at = self.buffer.at_row(last_row, false);
                self.buffer.paste(at, entry, false, count)
            }
            EntryKind::Blockwise => {
                self.cut_spans(&spans);
                let at = self.buffer.clamped(Cursor::at(top_left), true);
                self.buffer.paste(at, entry, true, count)
            }
        };

        self.session.mode = Mode::Normal;
        *self.selections = Selections::single(landed);
    }

    /// Cuts every span, bottom to top: a cut shifts everything below it and
    /// nothing above, so descending order keeps the rest valid.
    fn cut_spans(&mut self, spans: &[(usize, usize)]) {
        for &(start, end) in spans.iter().rev() {
            if end > start {
                self.buffer.operate_range(Cursor::at(start), Operator::Delete, start, end, false);
            }
        }
    }

    /// The same walk, replacing each span with `entry` rather than emptying it.
    fn replace_spans(&mut self, spans: &[(usize, usize)], entry: &Entry, count: usize) {
        for &(start, end) in spans.iter().rev() {
            self.buffer.paste_over(start, end, false, entry, count);
        }
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
                let Some(entry) = self.paste_source(*sink) else { return };
                let (before, count) = (*before, *count);
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.paste(sel.head, &entry, before, count))
                });
            }
            Action::PasteSelection { capture, count, sink } => {
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
            Action::SurroundSelection { with } => {
                let with = *with;
                let Some(pair) = crate::surround::pair_for(with) else {
                    self.session.status = format!("nothing surrounds with {with}");
                    return;
                };
                let linewise = self.session.mode.visual() == Some(VisualKind::Line);
                self.for_each_selection(|ed, sel| {
                    let len = ed.buffer.rope().len_chars();
                    let (start, end) = match linewise {
                        true => {
                            let (lo, hi) = sel.range();
                            ed.buffer.line_range(lo, hi, false)
                        }
                        false => sel.inclusive_range(len),
                    };
                    Selection::collapsed(ed.buffer.surround(sel.head, start, end, &pair))
                });
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
            | Action::LabelChar(_)
            | Action::LabelCancel
            | Action::ShowScopes
            | Action::EnterFind
            | Action::FindChar(_)
            | Action::FindBackspace
            | Action::FindCancel
            | Action::Buffer(_)
            | Action::Window(_)
            | Action::Tree(_) => {}
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
        self.session.picker = Some(Picker::new(kind, items, REGISTER_MIN_LEN));
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

    /// `:[range]m {address}` — vim's move, arithmetic and all.
    ///
    /// The range says which lines; with none written, the selection does,
    /// which is what makes `Shift-Down` and a bare `:m +1` agree about what
    /// they are moving.
    fn move_to(&mut self, lines_named: Option<LineRange>, to: Address) {
        let at = self.line_numbers();
        let (first, last) = match lines_named {
            Some(range) => match range.rows(at) {
                Ok(rows) => rows,
                Err(message) => {
                    self.session.status = message;
                    return;
                }
            },
            None => self.selected_rows(),
        };
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

    /// `:case {style}` — respells what is selected, or the word under the
    /// cursor when nothing is.
    ///
    /// The word is the fallback because renaming one is what this is for, and
    /// selecting it first is a keystroke that says nothing new. Every
    /// selection gets it, so a column of cursors respells a column of names.
    fn recase(&mut self, style: crate::case::Style) {
        let mut missed = false;
        self.for_each_selection(|ed, sel| {
            let len = ed.buffer.rope().len_chars();
            let (start, end) = match sel.is_collapsed() {
                false => sel.inclusive_range(len),
                // `iw` on whitespace is the run of whitespace, which is
                // right for `diw` and is not a word to respell.
                true => match ed
                    .buffer
                    .word_at(sel.head)
                    .filter(|&(a, b)| ed.buffer.slice(a, b).chars().any(char::is_alphanumeric))
                {
                    Some(range) => range,
                    None => {
                        missed = true;
                        return sel;
                    }
                },
            };
            let text = crate::case::convert(&ed.buffer.slice(start, end), style);
            ed.buffer.replace_range(start, end, &text);
            // On the first character of what was respelled: the text under the
            // old position is a different length now, and the start is the one
            // place that means the same thing either way.
            Selection::collapsed(ed.buffer.clamped(Cursor::at(start), false))
        });
        if missed {
            self.session.status = "no word under the cursor".into();
        }
        // A selection that has been rewritten is not a selection any more.
        self.session.mode = Mode::Normal;
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

    fn ex(ed: &mut Editor, line: &str) {
        ed.run_ex(line);
    }

    /// Out of the box, before any config is loaded.
    #[test]
    fn the_default_theme_is_gruvbox_dark() {
        let ed = Editor::empty();
        assert_eq!(ed.session.options.theme, crate::theme::DEFAULT_THEME);
        assert_eq!(ed.theme(), &Theme::default());
        assert!(ed.theme().ui.background.is_some(), "gruvbox claims the background");
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

        ex(&mut ed, "set theme gruvbox-dark");
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
        assert_eq!(ed.theme(), &Theme::default());
    }

    #[test]
    fn a_config_file_can_name_the_theme() {
        let mut ed = Editor::empty();
        let problems = ed.load_config(ConfigText(Some("[options]\ntheme = \"ansi\"\n")));
        assert_eq!(problems, []);
        assert_eq!(ed.theme().ui.background, None, "ansi leaves the terminal's alone");
    }

    #[test]
    fn a_theme_that_is_not_a_string_is_reported_and_not_fatal() {
        let mut ed = Editor::empty();
        let problems = ed.load_config(ConfigText(Some("[options]\ntheme = 7\n")));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.contains("theme"), "{:?}", problems[0].message);
        assert_eq!(ed.theme(), &Theme::default());
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
        ed.apply(cmd(Action::EnterVisual(VisualKind::Line)));
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
        for line in ["1,5w out.txt", "2q", "%case snake"] {
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
            ed.apply(cmd(Action::EnterVisual(VisualKind::Line)));
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
        ed.apply(cmd(Action::EnterVisual(VisualKind::Line)));
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

        // The tree goes between the two files, so a single step reaches it
        // from either one — which is what makes "the window you came from"
        // the thing under test rather than the cycling order.
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
        ed.apply(cmd(Action::Window(WindowCmd::Cycle { back: true })));
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

    /// `gf` in a tree is the same question as `gf` in a text window — go to a
    /// thing by name — over the things this pane has. Taking one moves the
    /// selection to it and opens nothing.
    #[test]
    fn gf_in_a_tree_finds_a_row_and_selects_it() {
        let d = ScratchDir::new("tree-pick").file("alpha.rs").file("beta.rs").file("gamma.rs");
        let mut ed = Editor::open(d.path()).unwrap();
        assert_eq!(ed.window().tree().unwrap().selected(), 0, "on the root");

        ed.apply(cmd(Action::OpenPicker(PickerKind::TreeRow)));
        assert_eq!(ed.session.mode, Mode::Pick);
        let listed: Vec<String> =
            ed.session.picker.as_ref().unwrap().items().iter().map(|i| i.text.clone()).collect();
        assert_eq!(listed, ["alpha.rs", "beta.rs", "gamma.rs"], "the rows, not the root");

        ed.apply(cmd(Action::PickChar('g')));
        ed.apply(cmd(Action::PickAccept));

        let tree = ed.window().tree().expect("still a tree, because nothing was opened");
        assert_eq!(tree.selected_row().unwrap().name, "gamma.rs");
        assert_eq!(ed.session.mode, Mode::Normal);
    }

    /// A directory is a tree item and `Ctrl-P` cannot reach one, which is half
    /// the reason this list is the rows and not the filesystem. Only the rows,
    /// though: what a collapsed directory hides is not on this pane.
    #[test]
    fn the_tree_picker_offers_directories_and_only_what_is_expanded() {
        let d = ScratchDir::new("tree-pick-dirs").dir("pkg").file("pkg/deep.rs").file("top.rs");
        let mut ed = Editor::open(d.path()).unwrap();

        let listed = |ed: &Editor| -> Vec<String> {
            ed.session.picker.as_ref().unwrap().items().iter().map(|i| i.text.clone()).collect()
        };
        ed.apply(cmd(Action::OpenPicker(PickerKind::TreeRow)));
        assert_eq!(listed(&ed), ["pkg", "top.rs"], "the closed directory hides its file");
        ed.apply(cmd(Action::PickCancel));

        select_first_entry(&mut ed);
        tree_key(&mut ed, TreeCmd::Expand);
        ed.apply(cmd(Action::OpenPicker(PickerKind::TreeRow)));

        assert_eq!(listed(&ed), ["pkg", "pkg/deep.rs", "top.rs"], "and shows it once open");
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
        ed.apply(cmd(Action::Paste { before: false, count: 1, sink: Sink::Ring }));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "abcdebcf\nghijkhil\nmnopqnor");
    }

    #[test]
    fn pasting_a_block_pads_short_rows_and_grows_the_buffer() {
        let mut ed = editor("xy");
        ed.session.registers.push(Entry { text: "bc\nhi\nno".into(), kind: EntryKind::Blockwise });
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
        let mut ed = visual(text, at, VisualKind::Char);
        for _ in 1..chars {
            ed.apply(cmd(Action::Move(Motion::Right)));
        }
        ed.session.registers.push(entry);
        ed
    }

    fn charwise(text: &str) -> Entry {
        Entry { text: text.into(), kind: EntryKind::Charwise }
    }

    fn linewise(text: &str) -> Entry {
        Entry { text: text.into(), kind: EntryKind::Linewise }
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
        let mut ed = visual("one two\nthree\nfour", 8, VisualKind::Line);
        ed.session.registers.push(charwise("one"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one two\none\nfour");
        assert_eq!(heads(&ed), vec![8]);
    }

    #[test]
    fn a_linewise_entry_over_a_linewise_selection_replaces_the_lines() {
        let mut ed = visual("one\ntwo\nthree", 4, VisualKind::Line);
        ed.session.registers.push(linewise("one\n"));
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one\none\nthree");
        assert_eq!(ed.session.registers.front().unwrap().text, "two\n", "captured linewise");
    }

    /// A file that ended without a newline still does.
    #[test]
    fn pasting_over_the_last_line_invents_no_terminator() {
        let mut ed = visual("one\ntwo", 4, VisualKind::Line);
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
        let mut ed = visual("one two", 4, VisualKind::Char);
        ed.apply(paste_over(true, 1));
        assert_eq!(ed.buffer().unwrap().rope().to_string(), "one two");
        assert_eq!(ed.session.status, "nothing to paste");
        assert_eq!(ed.session.mode, Mode::Visual(VisualKind::Char), "still selecting");
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
        let mut ed = visual("one two", 4, VisualKind::Char);
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
        let mut ed =
            ready("ab\ncd\nefgh", 6, 2, Entry { text: "a\nc".into(), kind: EntryKind::Blockwise });
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
            EntryKind::Blockwise,
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
        ed.session.registers.push(Entry { text: "ab\nef".into(), kind: EntryKind::Blockwise });
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
        let mut ed = visual("one two", 4, VisualKind::Char);
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
        let mut ed = visual("one two", 4, VisualKind::Char);
        ed.session.registers.push(charwise("one"));
        ed.apply(cmd(Action::OpenPicker(PickerKind::Register { before: false })));
        ed.apply(cmd(Action::PickCancel));
        assert_eq!(ed.session.mode, Mode::Visual(VisualKind::Char));
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
        ed.apply(cmd(Action::EnterVisual(VisualKind::Line)));
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
        assert_eq!(ed.session.mode, Mode::Visual(VisualKind::Line), "still selecting");
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
        ed.apply(cmd(Action::EnterVisual(VisualKind::Block)));
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

    #[test]
    fn visual_s_wraps_the_selection_and_leaves_visual_mode() {
        let mut ed = editor("hello there");
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
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
        ed.apply(cmd(Action::EnterVisual(VisualKind::Char)));
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
        // Row 2 of 0..5, and column 8: the pane is 21 wide with a two-cell
        // gutter for a five-line file, so the three-wide block starts at
        // (19 - 3) / 2.
        let letter = label_of_window(&ed, focus);
        assert_eq!(
            box_of,
            [(1, 8, "   ".into()), (2, 8, format!(" {letter} ")), (3, 8, "   ".into())],
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

        assert_eq!(ed.session.mode, Mode::Visual(VisualKind::Char));
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
}
