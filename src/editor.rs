//! Editor state and the action dispatch table.
//!
//! [`Action`] is the seam. Today `input.rs` is the only thing that produces
//! actions and the keymap is hardcoded; when a config language shows up, it
//! produces actions too and nothing here changes.

use std::path::Path;

use anyhow::Result;

use crate::buffer::{Buffer, Cursor};
use crate::motion::{Motion, Operator, Target};
use crate::picker::{Item, Picker, PickerKind};
use crate::registers::{EntryKind, Registers, Sink};
use crate::selection::{Selection, Selections};
use crate::syntax::Syntax;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    /// The `:` line being typed, without the leading colon.
    Command(String),
    /// The picker overlay is up. Its state lives in `Editor::picker` — a
    /// `Picker` is far too large to sit inside this enum.
    Pick,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command(_) => "COMMAND",
            Mode::Pick => "PICK",
        }
    }

    /// Whether the cursor may rest one past the last char of a line.
    pub fn allows_eol(&self) -> bool {
        matches!(self, Mode::Insert)
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

pub struct Editor {
    pub buffer: Buffer,
    /// Where everyone is looking. Normal mode is every selection collapsed;
    /// visual is one with room in it; multi-cursor is more than one. Here
    /// rather than on `Buffer` so a second view of the same file is possible —
    /// see `docs/specs/selections.md`.
    pub selections: Selections,
    /// Global, not per-buffer: yanking in one file and pasting in another is
    /// the point, so they outlive any single buffer.
    pub registers: Registers,
    pub picker: Option<Picker>,
    /// The parse tree, when the file's extension has a grammar.
    ///
    /// Here rather than on `Buffer` because `pending_edits` will have two
    /// consumers — tree-sitter now, LSP `didChange` later — and whoever drains
    /// it destroys it for the other. One drain point feeds both.
    pub syntax: Option<Syntax>,
    pub mode: Mode,
    /// The last `f`/`F`/`t`/`T`, for `;` and `,` to repeat. Here rather than in
    /// the keymap because it must outlive `Input::reset()`.
    last_find: Option<Motion>,
    pub status: String,
    /// First visible row.
    pub scroll: usize,
    pub quit: bool,
}

impl Editor {
    pub fn empty() -> Self {
        Self::with_buffer(Buffer::empty())
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut editor = Self::with_buffer(Buffer::open(path)?);
        editor.reload_syntax();
        Ok(editor)
    }

    /// Picks a grammar from the file's extension. An unknown one leaves
    /// `syntax` as `None`, which renders as plain text.
    fn reload_syntax(&mut self) {
        let extension = self
            .buffer
            .path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_string();
        self.syntax = Syntax::new(&extension, self.buffer.rope());
    }

    /// Drains the edit log into the parse tree. Called once per key, after the
    /// command has been applied and before the frame is drawn.
    pub fn sync_syntax(&mut self) {
        let edits = std::mem::take(&mut self.buffer.pending_edits);
        if edits.is_empty() {
            return;
        }
        if let Some(syntax) = &mut self.syntax {
            syntax.update(self.buffer.rope(), &edits);
        }
    }

    fn with_buffer(buffer: Buffer) -> Self {
        Self {
            buffer,
            selections: Selections::default(),
            registers: Registers::default(),
            picker: None,
            syntax: None,
            mode: Mode::Normal,
            last_find: None,
            status: String::new(),
            scroll: 0,
            quit: false,
        }
    }

    pub fn apply(&mut self, cmd: Command) {
        let n = if cmd.action.repeatable() { cmd.count.max(1) } else { 1 };
        for _ in 0..n {
            self.apply_once(&cmd.action);
        }

        // One command is one undo step, so the group closes here rather than
        // inside the count loop — `5x` comes back in a single `u`. Insert mode
        // is the exception: the group stays open until Esc, which is what makes
        // a typing run (and the `\n` that `o` inserted before it) undo together.
        // One command is one undo step even across N selections, so the group
        // closes after the whole pass rather than per selection — otherwise `u`
        // would walk back through a multi-cursor edit one cursor at a time.
        if self.mode != Mode::Insert {
            let at = self.selections.cursor();
            self.buffer.commit_undo(at);
        }
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
    fn for_each_selection(&mut self, mut f: impl FnMut(&mut Editor, Selection) -> Selection) {
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
        self.selections = Selections::single(cursor);
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
                self.last_find = Some(motion);
                Some(motion)
            }
            Motion::RepeatFind { reverse } => match self.last_find {
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
        let eol = self.mode.allows_eol();

        match action {
            Action::Move(m) => {
                let Some(m) = self.resolve_find(*m) else { return };
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.moved(sel.head, m, eol))
                });
            }
            Action::Operate { op, target, count, sink } => {
                let Some(target) = self.resolve_find_target(*target) else { return };
                let (op, count, sink) = (*op, *count, *sink);
                self.for_each_selection(|ed, sel| {
                    match ed.buffer.operate(sel.head, op, target, count) {
                        Some((entry, landed)) => {
                            if sink == Sink::Ring {
                                ed.registers.push(entry);
                            }
                            Selection::collapsed(landed)
                        }
                        None => sel,
                    }
                });
                if op == Operator::Change {
                    self.mode = Mode::Insert;
                }
            }
            Action::Paste { before, count } => {
                // Cloned because pasting borrows the buffer mutably while the
                // entry is still owned by the ring.
                let Some(entry) = self.registers.front().cloned() else {
                    self.status = "nothing to paste".into();
                    return;
                };
                let (before, count) = (*before, *count);
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.paste(sel.head, &entry, before, count))
                });
            }

            Action::OpenPicker(kind) => self.open_picker(*kind),
            Action::PickChar(c) => {
                if let Some(p) = &mut self.picker {
                    p.push_char(*c);
                }
            }
            Action::PickBackspace => {
                // Backspacing off the front cancels, as it does on a `:` line.
                let empty = self.picker.as_mut().is_some_and(|p| !p.backspace());
                if empty {
                    self.close_picker();
                }
            }
            Action::PickNext => {
                if let Some(p) = &mut self.picker {
                    p.next();
                }
            }
            Action::PickPrev => {
                if let Some(p) = &mut self.picker {
                    p.prev();
                }
            }
            Action::PickToggleShort => {
                if let Some(p) = &mut self.picker {
                    p.toggle_short();
                }
            }
            Action::PickCancel => self.close_picker(),
            Action::PickAccept => self.accept_pick(),

            Action::EnterInsert => self.mode = Mode::Insert,
            Action::EnterInsertAfter => {
                // `a` may step onto the position just past the last char, which
                // normal mode forbids — so switch modes first.
                self.mode = Mode::Insert;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.moved(sel.head, Motion::Right, true))
                });
            }
            Action::EnterInsertLineStart => {
                self.mode = Mode::Insert;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.moved(sel.head, Motion::LineStart, true))
                });
            }
            Action::EnterInsertLineEnd => {
                self.mode = Mode::Insert;
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.moved(sel.head, Motion::LineEnd, true))
                });
            }
            Action::EnterNormal => {
                self.mode = Mode::Normal;
                // Matches vim: leaving insert pulls the cursor back onto a char.
                self.for_each_selection(|ed, sel| {
                    Selection::collapsed(ed.buffer.clamped(sel.head, false))
                });
            }
            Action::OpenLineBelow | Action::OpenLineAbove => {
                let below = matches!(action, Action::OpenLineBelow);
                self.mode = Mode::Insert;
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
                    self.status = "not enough characters on the line".into();
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
            Action::Undo => match self.buffer.undo(self.selections.cursor()) {
                Some(cursor) => self.selections = Selections::single(cursor),
                None => self.status = "already at oldest change".into(),
            },
            Action::Redo => match self.buffer.redo(self.selections.cursor()) {
                Some(cursor) => self.selections = Selections::single(cursor),
                None => self.status = "already at newest change".into(),
            },

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

            Action::EnterCommandMode => {
                self.status.clear();
                self.mode = Mode::Command(String::new());
            }
            Action::CommandChar(c) => {
                if let Mode::Command(line) = &mut self.mode {
                    line.push(*c);
                }
            }
            Action::CommandBackspace => {
                if let Mode::Command(line) = &mut self.mode {
                    if line.pop().is_none() {
                        self.mode = Mode::Normal;
                    }
                }
            }
            Action::CommandCancel => self.mode = Mode::Normal,
            Action::CommandExecute => {
                let line = match &self.mode {
                    Mode::Command(line) => line.clone(),
                    _ => return,
                };
                self.mode = Mode::Normal;
                self.run_ex(&line);
            }
        }
    }

    fn open_picker(&mut self, kind: PickerKind) {
        if self.registers.is_empty() {
            // An empty overlay is a worse answer than saying so.
            self.status = "nothing to paste".into();
            return;
        }
        let items = self
            .registers
            .iter()
            .map(|e| Item {
                text: e.text.clone(),
                badge: (e.kind == EntryKind::Linewise).then_some('¶'),
            })
            .collect();
        self.picker = Some(Picker::new(kind, items));
        self.mode = Mode::Pick;
    }

    fn close_picker(&mut self) {
        self.picker = None;
        self.mode = Mode::Normal;
    }

    fn accept_pick(&mut self) {
        let picker = self.picker.take();
        self.mode = Mode::Normal;
        let Some(picker) = picker else { return };
        let Some(entry) = picker.selected().and_then(|i| self.registers.get(i)).cloned() else {
            return;
        };
        match picker.kind {
            PickerKind::Register { before } => {
                // Push before pasting: move-to-front makes this the ring's head,
                // so `.` and a later bare `p` repeat the entry you chose rather
                // than whatever happened to be most recent.
                self.registers.push(entry.clone());
                let landed = self.buffer.paste(self.selections.cursor(), &entry, before, 1);
                self.selections = Selections::single(landed);
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
            "w" | "write" => {
                self.write(arg);
            }
            "q" | "quit" => self.quit(force),
            "e" | "edit" => self.edit(arg, force),
            "wq" | "x" => {
                if self.write(arg) {
                    self.quit(true);
                }
            }
            _ => {
                if let Ok(n) = name.parse::<usize>() {
                    let cursor = self.buffer.at_row(n.saturating_sub(1), false);
                    self.selections = Selections::single(cursor);
                } else {
                    self.status = format!("not a command: {name}");
                }
            }
        }
    }

    /// Returns whether the write succeeded.
    fn write(&mut self, path: &str) -> bool {
        let at = self.selections.cursor();
        let result =
            if path.is_empty() { self.buffer.save(at) } else { self.buffer.save_as(at, path) };
        match result {
            Ok(()) => {
                // `:w other.rs` can change the language under us.
                if !path.is_empty() {
                    self.reload_syntax();
                }
                let name =
                    self.buffer.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.status = format!("\"{name}\" written");
                true
            }
            Err(e) => {
                self.status = format!("error: {e:#}");
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
            self.status = "unsaved changes (use `:e!` to discard)".into();
            return;
        }

        let at = self.selections.cursor();
        let result = if path.is_empty() {
            self.buffer.reload(at)
        } else {
            Buffer::open(path).map(|buf| {
                self.buffer = buf;
                // A different file, so the old position means nothing.
                Cursor::default()
            })
        };

        match result {
            Ok(cursor) => {
                self.selections = Selections::single(cursor);
                self.reload_syntax();
                let name =
                    self.buffer.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                self.status = format!("\"{name}\" loaded");
            }
            Err(e) => self.status = format!("{e:#}"),
        }
    }

    fn quit(&mut self, force: bool) {
        if self.buffer.is_modified() && !force {
            self.status = "unsaved changes (use `:q!` to discard)".into();
        } else {
            self.quit = true;
        }
    }

    /// Keeps the cursor inside a `height`-row viewport, with a scrolloff margin.
    pub fn scroll_to_cursor(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        const SCROLLOFF: usize = 3;
        let row = self.buffer.row_at(self.selections.cursor());
        let margin = SCROLLOFF.min(height.saturating_sub(1) / 2);

        if row < self.scroll + margin {
            self.scroll = row.saturating_sub(margin);
        } else if row + margin >= self.scroll + height {
            self.scroll = row + margin + 1 - height;
        }

        let max_scroll = self.buffer.line_count().saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
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
            let at = ed.buffer.insert_str(ed.cursor(), text);
            ed.buffer.commit_undo(at);
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
        assert_eq!(ed.buffer.rope().to_string(), "f");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "abcdef", "5x is one unit, not five");
    }

    #[test]
    fn a_whole_insert_session_undoes_as_one_unit() {
        let mut ed = editor("");
        ed.apply(cmd(Action::EnterInsert));
        type_str(&mut ed, "hello");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer.rope().to_string(), "hello");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "", "all five chars, not one");
    }

    /// `o` edits *and* enters insert mode. The newline it inserts belongs to the
    /// same undo unit as everything typed after it.
    #[test]
    fn open_line_and_what_follows_it_undo_together() {
        let mut ed = editor("a");
        ed.apply(cmd(Action::OpenLineBelow));
        type_str(&mut ed, "bc");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer.rope().to_string(), "a\nbc");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "a", "the newline went back too");
    }

    #[test]
    fn entering_and_leaving_insert_without_typing_is_not_an_undo_step() {
        let mut ed = editor("a");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(cmd(Action::EnterInsert));
        ed.apply(cmd(Action::EnterNormal));

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "a", "one undo reaches the delete");
    }

    #[test]
    fn undo_takes_a_count() {
        let mut ed = editor("abcdef");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        assert_eq!(ed.buffer.rope().to_string(), "def");

        ed.apply(Command { count: 3, action: Action::Undo });
        assert_eq!(ed.buffer.rope().to_string(), "abcdef");
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
        assert_eq!(ed.buffer.rope().to_string(), "baz");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "foo bar baz", "both words, one undo");
    }

    #[test]
    fn c_enters_insert_mode_so_you_can_type_the_replacement() {
        let mut ed = editor("foo bar");
        ed.apply(operate(Operator::Change, Motion::WordForward, 1));
        assert_eq!(ed.mode, Mode::Insert);
        assert_eq!(ed.buffer.rope().to_string(), " bar");

        type_str(&mut ed, "xyz");
        ed.apply(cmd(Action::EnterNormal));
        assert_eq!(ed.buffer.rope().to_string(), "xyz bar");
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
        assert_eq!(ed.buffer.rope().to_string(), "foo bar");
    }

    #[test]
    fn a_delete_that_matches_nothing_leaves_no_undo_step() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(operate(Operator::Delete, Motion::WordBackward, 1));
        assert_eq!(ed.buffer.rope().to_string(), "bc", "b at char 0 did nothing");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "abc", "one undo still reaches the x");
    }

    fn paste(before: bool, count: usize) -> Command {
        cmd(Action::Paste { before, count })
    }

    #[test]
    fn yank_then_paste_round_trips() {
        let mut ed = editor("foo bar");
        ed.apply(operate(Operator::Yank, Motion::WordForward, 1));
        assert_eq!(ed.buffer.rope().to_string(), "foo bar", "yank changed nothing");

        ed.apply(paste(false, 1));
        assert_eq!(ed.buffer.rope().to_string(), "ffoo oo bar");
    }

    #[test]
    fn a_delete_fills_the_ring_so_p_puts_it_back() {
        let mut ed = editor("one\ntwo");
        ed.apply(operate(Operator::Delete, Motion::CurrentLine, 1));
        assert_eq!(ed.buffer.rope().to_string(), "two");

        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer.rope().to_string(), "one\ntwo", "linewise, so above");
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
        assert_eq!(ed.buffer.rope().to_string(), "keep", "the junk line is gone");

        ed.apply(paste(false, 1));
        assert_eq!(
            ed.buffer.rope().to_string(),
            "keep\nkeep",
            "the ring still holds the yank, not the junk"
        );
    }

    #[test]
    fn pasting_from_an_empty_ring_says_so() {
        let mut ed = editor("abc");
        ed.apply(paste(false, 1));
        assert_eq!(ed.buffer.rope().to_string(), "abc");
        assert_eq!(ed.status, "nothing to paste");
    }

    #[test]
    fn a_paste_is_one_undo_step_even_with_a_count() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Yank, Motion::Right, 1));
        ed.apply(paste(false, 3));
        assert_eq!(ed.buffer.rope().to_string(), "aaaabc");

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "abc");
    }

    /// Undo puts the text back in the buffer and leaves the ring alone, as vim
    /// does — you can undo a delete and still paste what it took.
    #[test]
    fn undo_does_not_roll_back_the_ring() {
        let mut ed = editor("one\ntwo");
        ed.apply(operate(Operator::Delete, Motion::CurrentLine, 1));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "one\ntwo");

        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer.rope().to_string(), "one\none\ntwo");
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
            let c = ed.buffer.at_row(row, false);
            ed.set_cursor(c);
            ed.apply(operate(Operator::Yank, Motion::CurrentLine, 1));
        }
        ed
    }

    #[test]
    fn the_picker_opens_over_the_ring_most_recent_first() {
        let mut ed = ed_with_ring();
        ed.apply(open_register_picker(false));

        assert_eq!(ed.mode, Mode::Pick);
        let p = ed.picker.as_ref().unwrap();
        assert_eq!(p.items()[p.selected().unwrap()].text, "gamma\n");
    }

    #[test]
    fn an_empty_ring_reports_instead_of_opening_an_empty_overlay() {
        let mut ed = editor("abc");
        ed.apply(open_register_picker(false));

        assert_eq!(ed.mode, Mode::Normal);
        assert!(ed.picker.is_none());
        assert_eq!(ed.status, "nothing to paste");
    }

    #[test]
    fn accepting_pastes_the_chosen_entry_not_the_most_recent() {
        let mut ed = ed_with_ring();
        let c = ed.buffer.at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickNext, Action::PickAccept]);

        assert_eq!(ed.mode, Mode::Normal);
        assert!(ed.picker.is_none());
        assert_eq!(
            ed.buffer.rope().to_string(),
            "alpha\nalpha\nbeta\ngamma",
            "the third-newest entry, chosen by moving down twice"
        );
    }

    #[test]
    fn typing_in_the_picker_filters_what_accept_takes() {
        let mut ed = ed_with_ring();
        let c = ed.buffer.at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickChar('b'), Action::PickChar('e'), Action::PickAccept]);
        assert_eq!(ed.buffer.rope().to_string(), "beta\nalpha\nbeta\ngamma");
    }

    /// Choosing promotes the entry, so a plain `p` afterwards repeats it — this
    /// is what makes `.` work without re-opening the picker.
    #[test]
    fn accepting_moves_the_entry_to_the_front_of_the_ring() {
        let mut ed = ed_with_ring();
        let c = ed.buffer.at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickNext, Action::PickAccept]);

        assert_eq!(ed.registers.front().unwrap().text, "alpha\n");
        ed.apply(paste(true, 1));
        assert_eq!(ed.buffer.rope().to_string(), "alpha\nalpha\nalpha\nbeta\ngamma");
    }

    #[test]
    fn cancelling_leaves_the_buffer_and_the_ring_alone() {
        let mut ed = ed_with_ring();
        let before = ed.buffer.rope().to_string();
        ed.apply(open_register_picker(false));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickCancel]);

        assert_eq!(ed.mode, Mode::Normal);
        assert!(ed.picker.is_none());
        assert_eq!(ed.buffer.rope().to_string(), before);
        assert_eq!(ed.registers.front().unwrap().text, "gamma\n");
    }

    #[test]
    fn backspacing_an_empty_query_closes_the_picker() {
        let mut ed = ed_with_ring();
        ed.apply(open_register_picker(false));
        ed.apply(cmd(Action::PickChar('a')));
        ed.apply(cmd(Action::PickBackspace));
        assert_eq!(ed.mode, Mode::Pick, "still open, one char removed");

        ed.apply(cmd(Action::PickBackspace));
        assert_eq!(ed.mode, Mode::Normal, "nothing left to delete");
    }

    #[test]
    fn a_picked_paste_is_one_undo_step() {
        let mut ed = ed_with_ring();
        let before = ed.buffer.rope().to_string();
        let c = ed.buffer.at_row(0, false);
        ed.set_cursor(c);
        ed.apply(open_register_picker(true));
        pick_keys(&mut ed, &[Action::PickNext, Action::PickAccept]);
        assert_ne!(ed.buffer.rope().to_string(), before);

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), before);
    }

    #[test]
    fn redo_walks_back_forward() {
        let mut ed = editor("abc");
        ed.apply(operate(Operator::Delete, Motion::Right, 1));
        ed.apply(cmd(Action::Undo));
        ed.apply(cmd(Action::Redo));
        assert_eq!(ed.buffer.rope().to_string(), "bc");
    }

    #[test]
    fn the_ends_of_the_history_report_themselves() {
        let mut ed = editor("a");
        ed.apply(cmd(Action::Undo));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.status, "already at oldest change");

        ed.status.clear();
        ed.apply(cmd(Action::Redo));
        ed.apply(cmd(Action::Redo));
        assert_eq!(ed.status, "already at newest change");
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
        assert_eq!(ed.buffer.rope().to_string(), "before\n");

        f.write("after\n");
        ex(&mut ed, "e");
        assert_eq!(ed.buffer.rope().to_string(), "after\n");
    }

    #[test]
    fn e_refuses_to_discard_unsaved_changes() {
        let f = Scratch::new("guard.txt", "on disk\n");
        let mut ed = opened(&f);
        type_str(&mut ed, "local edit");
        f.write("changed underneath\n");

        ex(&mut ed, "e");
        assert!(ed.status.contains("unsaved changes"), "got: {}", ed.status);
        assert!(
            ed.buffer.rope().to_string().contains("local edit"),
            "the buffer must be left alone when the reload is refused",
        );
    }

    #[test]
    fn e_bang_discards_them() {
        let f = Scratch::new("force.txt", "on disk\n");
        let mut ed = opened(&f);
        type_str(&mut ed, "local edit");

        ex(&mut ed, "e!");
        assert_eq!(ed.buffer.rope().to_string(), "on disk\n");
        assert!(!ed.buffer.is_modified(), "a fresh read is not a modified buffer");
    }

    #[test]
    fn a_reload_drops_undo_history_rather_than_replaying_gone_text() {
        let f = Scratch::new("history.txt", "one\n");
        let mut ed = opened(&f);
        type_str(&mut ed, "typed");
        ed.buffer.commit_undo(ed.cursor());

        f.write("two\n");
        ex(&mut ed, "e!");
        assert_eq!(ed.buffer.rope().to_string(), "two\n");

        // Undoing here must not resurrect text from the previous file.
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "two\n");
    }

    #[test]
    fn e_with_a_path_edits_that_file_instead() {
        let a = Scratch::new("a.txt", "file a\n");
        let b = Scratch::new("b.txt", "file b\n");
        let mut ed = opened(&a);

        ex(&mut ed, &format!("e {}", b.path()));
        assert_eq!(ed.buffer.rope().to_string(), "file b\n");
        assert_eq!(ed.buffer.path.as_deref(), Some(std::path::Path::new(b.path())));
    }

    #[test]
    fn a_shorter_file_does_not_leave_the_cursor_past_the_end() {
        let f = Scratch::new("shrink.txt", "one\ntwo\nthree\nfour\n");
        let mut ed = opened(&f);
        let c = ed.buffer.at_row(3, false);
        ed.set_cursor(c);

        f.write("x\n");
        ex(&mut ed, "e!");
        assert!(
            ed.cursor().at <= ed.buffer.rope().len_chars(),
            "cursor {} is past the end of a {}-char buffer",
            ed.cursor().at,
            ed.buffer.rope().len_chars(),
        );
        assert_eq!(ed.cursor_row(), 0);
    }

    #[test]
    fn a_reload_rebuilds_the_parse_tree_rather_than_patching_it() {
        let f = Scratch::new("tree.rs", "fn a() {}\n");
        let mut ed = opened(&f);
        assert!(ed.syntax.is_some(), "a .rs file should have a grammar");

        f.write("struct B;\n");
        ex(&mut ed, "e!");

        // A tree left over from the old text would disagree with the rope.
        let rope = ed.buffer.rope();
        let spans = ed.syntax.as_ref().unwrap().highlights(rope, 0..rope.len_bytes());
        assert!(
            spans.iter().all(|s| s.end_byte <= rope.len_bytes()),
            "highlight spans point past the end of the reloaded text",
        );
    }

    #[test]
    fn e_on_a_buffer_with_no_file_name_reports_rather_than_panicking() {
        let mut ed = editor("scratch");
        ed.buffer.commit_undo(ed.cursor());
        ex(&mut ed, "e");
        assert!(!ed.status.is_empty(), "should say something");
        assert_eq!(ed.buffer.rope().to_string(), "scratch");
    }

    // ---- commands across several selections --------------------------------
    //
    // Nothing binds a key to "add a cursor" yet — that is step 3 — but the
    // machinery is here, and these are the cases it exists to get right.

    fn with_cursors(text: &str, positions: &[usize]) -> Editor {
        let mut ed = editor(text);
        ed.selections.set(positions.iter().map(|&p| Selection::at(p)).collect());
        ed
    }

    fn heads(ed: &Editor) -> Vec<usize> {
        ed.selections.all().iter().map(|s| s.head.at).collect()
    }

    #[test]
    fn typing_inserts_at_every_cursor() {
        //                   0123456789
        let mut ed = with_cursors("aa bb cc", &[0, 3, 6]);
        ed.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        assert_eq!(ed.buffer.rope().to_string(), "Xaa Xbb Xcc");
    }

    /// The reason edits run highest-position-first: an insert at 0 shifts
    /// everything after it, so an ascending pass would put the later ones in
    /// the wrong place.
    #[test]
    fn later_cursors_are_not_shifted_by_earlier_edits() {
        let mut ed = with_cursors("....|....|", &[4, 9]);
        ed.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('#')));
        assert_eq!(ed.buffer.rope().to_string(), "....#|....#|");
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
        assert_eq!(ed.buffer.rope().to_string(), "  ");
    }

    #[test]
    fn a_multi_cursor_edit_is_one_undo_step() {
        let mut ed = with_cursors("aa bb cc", &[0, 3, 6]);
        ed.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        ed.mode = Mode::Normal;
        ed.apply(cmd(Action::EnterNormal));

        assert_eq!(ed.buffer.rope().to_string(), "Xaa Xbb Xcc");
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "aa bb cc", "one u, not one per cursor",);
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
        assert_eq!(ed.selections.len(), 1, "collided cursors are one cursor");
    }

    #[test]
    fn the_primary_cursor_is_what_the_viewport_and_status_line_follow() {
        let ed = with_cursors("one\ntwo\nthree", &[0, 8]);
        assert_eq!(ed.cursor().at, ed.selections.primary().head.at);
        assert!(ed.cursor_row() <= 2);
    }

    /// Undo restores a single cursor for now. Restoring the whole set is step 3
    /// of docs/specs/selections.md, where it first becomes observable.
    #[test]
    fn undo_collapses_to_one_cursor_for_now() {
        let mut ed = with_cursors("aa bb", &[0, 3]);
        ed.mode = Mode::Insert;
        ed.apply(cmd(Action::InsertChar('X')));
        ed.apply(cmd(Action::EnterNormal));
        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.selections.len(), 1);
    }
}
