//! Editor state and the action dispatch table.
//!
//! [`Action`] is the seam. Today `input.rs` is the only thing that produces
//! actions and the keymap is hardcoded; when a config language shows up, it
//! produces actions too and nothing here changes.

use std::path::Path;

use anyhow::Result;

use crate::buffer::Buffer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    /// The `:` line being typed, without the leading colon.
    Command(String),
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command(_) => "COMMAND",
        }
    }

    /// Whether the cursor may rest one past the last char of a line.
    pub fn allows_eol(&self) -> bool {
        matches!(self, Mode::Insert)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveWordForward,
    MoveWordBackward,
    MoveLineStart,
    MoveLineEnd,
    GotoFirstLine,
    GotoLastLine,
    /// 1-based, as the user typed it.
    GotoLine(usize),

    EnterInsert,
    EnterInsertAfter,
    EnterInsertLineStart,
    EnterInsertLineEnd,
    EnterNormal,
    OpenLineBelow,
    OpenLineAbove,

    DeleteChar,
    DeleteLine,

    Undo,
    Redo,

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
        matches!(
            self,
            Action::MoveLeft
                | Action::MoveRight
                | Action::MoveUp
                | Action::MoveDown
                | Action::MoveWordForward
                | Action::MoveWordBackward
                | Action::DeleteChar
                | Action::DeleteLine
                | Action::InsertChar(_)
                | Action::Undo
                | Action::Redo
        )
    }
}

#[derive(Debug, Clone)]
pub struct Command {
    pub count: usize,
    pub action: Action,
}

pub struct Editor {
    pub buffer: Buffer,
    pub mode: Mode,
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
        Ok(Self::with_buffer(Buffer::open(path)?))
    }

    fn with_buffer(buffer: Buffer) -> Self {
        Self {
            buffer,
            mode: Mode::Normal,
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
        if self.mode != Mode::Insert {
            self.buffer.commit_undo();
        }
    }

    fn apply_once(&mut self, action: &Action) {
        let eol = self.mode.allows_eol();

        match action {
            Action::MoveLeft => self.buffer.move_left(),
            Action::MoveRight => self.buffer.move_right(eol),
            Action::MoveUp => self.buffer.move_vertical(-1, eol),
            Action::MoveDown => self.buffer.move_vertical(1, eol),
            Action::MoveWordForward => self.buffer.move_word_forward(eol),
            Action::MoveWordBackward => self.buffer.move_word_backward(eol),
            Action::MoveLineStart => self.buffer.move_line_start(),
            Action::MoveLineEnd => self.buffer.move_line_end(eol),
            Action::GotoFirstLine => self.buffer.goto_row(0, eol),
            Action::GotoLastLine => self.buffer.goto_row(usize::MAX, eol),
            Action::GotoLine(n) => self.buffer.goto_row(n.saturating_sub(1), eol),

            Action::EnterInsert => self.mode = Mode::Insert,
            Action::EnterInsertAfter => {
                // `a` may step onto the position just past the last char, which
                // normal mode forbids — so switch modes first.
                self.mode = Mode::Insert;
                self.buffer.move_right(true);
            }
            Action::EnterInsertLineStart => {
                self.mode = Mode::Insert;
                self.buffer.move_line_start();
            }
            Action::EnterInsertLineEnd => {
                self.mode = Mode::Insert;
                self.buffer.move_line_end(true);
            }
            Action::EnterNormal => {
                self.mode = Mode::Normal;
                // Matches vim: leaving insert pulls the cursor back onto a char.
                self.buffer.clamp(false);
            }
            Action::OpenLineBelow => {
                self.mode = Mode::Insert;
                self.buffer.open_line(true);
            }
            Action::OpenLineAbove => {
                self.mode = Mode::Insert;
                self.buffer.open_line(false);
            }

            Action::DeleteChar => self.buffer.delete_char_forward(),
            Action::DeleteLine => self.buffer.delete_line(),

            Action::Undo => {
                if !self.buffer.undo() {
                    self.status = "already at oldest change".into();
                }
            }
            Action::Redo => {
                if !self.buffer.redo() {
                    self.status = "already at newest change".into();
                }
            }

            Action::InsertChar(c) => self.buffer.insert_char(*c),
            Action::InsertNewline => self.buffer.insert_char('\n'),
            Action::Backspace => self.buffer.backspace(),

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
            "wq" | "x" => {
                if self.write(arg) {
                    self.quit(true);
                }
            }
            _ => {
                if let Ok(n) = name.parse::<usize>() {
                    self.buffer.goto_row(n.saturating_sub(1), false);
                } else {
                    self.status = format!("not a command: {name}");
                }
            }
        }
    }

    /// Returns whether the write succeeded.
    fn write(&mut self, path: &str) -> bool {
        let result = if path.is_empty() {
            self.buffer.save()
        } else {
            self.buffer.save_as(path)
        };
        match result {
            Ok(()) => {
                let name = self
                    .buffer
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.status = format!("\"{name}\" written");
                true
            }
            Err(e) => {
                self.status = format!("error: {e:#}");
                false
            }
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
        let row = self.buffer.cursor_row();
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

    /// Text arrives as one committed revision, so a single undo lands back on
    /// it rather than on an empty buffer.
    fn editor(text: &str) -> Editor {
        let mut ed = Editor::empty();
        if !text.is_empty() {
            ed.buffer.insert_str(text);
            ed.buffer.commit_undo();
        }
        ed.buffer.cursor = 0;
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
        ed.apply(Command { count: 5, action: Action::DeleteChar });
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
        ed.apply(cmd(Action::DeleteChar));
        ed.apply(cmd(Action::EnterInsert));
        ed.apply(cmd(Action::EnterNormal));

        ed.apply(cmd(Action::Undo));
        assert_eq!(ed.buffer.rope().to_string(), "a", "one undo reaches the delete");
    }

    #[test]
    fn undo_takes_a_count() {
        let mut ed = editor("abcdef");
        ed.apply(cmd(Action::DeleteChar));
        ed.apply(cmd(Action::DeleteChar));
        ed.apply(cmd(Action::DeleteChar));
        assert_eq!(ed.buffer.rope().to_string(), "def");

        ed.apply(Command { count: 3, action: Action::Undo });
        assert_eq!(ed.buffer.rope().to_string(), "abcdef");
    }

    #[test]
    fn redo_walks_back_forward() {
        let mut ed = editor("abc");
        ed.apply(cmd(Action::DeleteChar));
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
}
