//! Key events → [`Command`].
//!
//! The keymap is hardcoded on purpose. Extracting it into config is a step-2
//! problem; doing it now would make the config format the project.
//!
//! [`Input::pending`] is the operator-pending slot. It currently handles only
//! `dd` and `gg`, but it's the hook where `d{motion}`, `c{motion}`, `y{motion}`
//! go — parse a motion after the operator instead of matching a second char.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::{Action, Command, Mode};

#[derive(Default)]
pub struct Input {
    count: Option<usize>,
    pending: Option<char>,
}

impl Input {
    pub fn on_key(&mut self, key: KeyEvent, mode: &Mode) -> Option<Command> {
        match mode {
            Mode::Normal => self.normal(key),
            Mode::Insert => Self::insert(key),
            Mode::Command(_) => Self::command_line(key),
        }
    }

    /// What's been typed but not yet resolved, for the status line.
    pub fn pending_display(&self) -> String {
        let mut s = String::new();
        if let Some(n) = self.count {
            s.push_str(&n.to_string());
        }
        if let Some(c) = self.pending {
            s.push(c);
        }
        s
    }

    fn reset(&mut self) {
        self.count = None;
        self.pending = None;
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1)
    }

    fn normal(&mut self, key: KeyEvent) -> Option<Command> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        let action = match key.code {
            KeyCode::Esc => {
                self.reset();
                return None;
            }
            KeyCode::Char('c') if ctrl => {
                self.reset();
                return None;
            }
            KeyCode::Char(c) => {
                // An operator is waiting: this key completes or cancels it.
                if let Some(op) = self.pending.take() {
                    let count = self.take_count();
                    return match (op, c) {
                        ('d', 'd') => Some(Command { count, action: Action::DeleteLine }),
                        ('g', 'g') => Some(Command { count: 1, action: Action::GotoFirstLine }),
                        _ => None,
                    };
                }

                // Digits build a count — except a leading `0`, which is a motion.
                if c.is_ascii_digit() && !(c == '0' && self.count.is_none()) {
                    let d = c.to_digit(10).unwrap() as usize;
                    self.count = Some(self.count.unwrap_or(0) * 10 + d);
                    return None;
                }

                match c {
                    'h' => Action::MoveLeft,
                    'l' | ' ' => Action::MoveRight,
                    'j' => Action::MoveDown,
                    'k' => Action::MoveUp,
                    'w' => Action::MoveWordForward,
                    'b' => Action::MoveWordBackward,
                    '0' => Action::MoveLineStart,
                    '^' => Action::MoveLineStart,
                    '$' => Action::MoveLineEnd,
                    'i' => Action::EnterInsert,
                    'a' => Action::EnterInsertAfter,
                    'I' => Action::EnterInsertLineStart,
                    'A' => Action::EnterInsertLineEnd,
                    'o' => Action::OpenLineBelow,
                    'O' => Action::OpenLineAbove,
                    'x' => Action::DeleteChar,
                    'd' | 'g' => {
                        self.pending = Some(c);
                        return None;
                    }
                    // `G` with a count is "go to line N", without one it's
                    // "go to the end" — so the count isn't a repeat here.
                    'G' => {
                        let explicit = self.count.take();
                        self.pending = None;
                        return Some(Command {
                            count: 1,
                            action: match explicit {
                                Some(n) => Action::GotoLine(n),
                                None => Action::GotoLastLine,
                            },
                        });
                    }
                    ':' => {
                        self.reset();
                        return Some(Command { count: 1, action: Action::EnterCommandMode });
                    }
                    _ => {
                        self.reset();
                        return None;
                    }
                }
            }
            KeyCode::Left => Action::MoveLeft,
            KeyCode::Right => Action::MoveRight,
            KeyCode::Down => Action::MoveDown,
            KeyCode::Up => Action::MoveUp,
            KeyCode::Home => Action::MoveLineStart,
            KeyCode::End => Action::MoveLineEnd,
            _ => {
                self.reset();
                return None;
            }
        };

        self.pending = None;
        let count = self.take_count();
        Some(Command { count, action })
    }

    fn insert(key: KeyEvent) -> Option<Command> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        let action = match key.code {
            KeyCode::Esc => Action::EnterNormal,
            KeyCode::Char('c') if ctrl => Action::EnterNormal,
            KeyCode::Char(c) => Action::InsertChar(c),
            KeyCode::Enter => Action::InsertNewline,
            KeyCode::Backspace => Action::Backspace,
            KeyCode::Tab => Action::InsertChar('\t'),
            KeyCode::Left => Action::MoveLeft,
            KeyCode::Right => Action::MoveRight,
            KeyCode::Down => Action::MoveDown,
            KeyCode::Up => Action::MoveUp,
            KeyCode::Home => Action::MoveLineStart,
            KeyCode::End => Action::MoveLineEnd,
            _ => return None,
        };
        Some(Command { count: 1, action })
    }

    fn command_line(key: KeyEvent) -> Option<Command> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        let action = match key.code {
            KeyCode::Esc => Action::CommandCancel,
            KeyCode::Char('c') if ctrl => Action::CommandCancel,
            KeyCode::Enter => Action::CommandExecute,
            KeyCode::Backspace => Action::CommandBackspace,
            KeyCode::Char(c) => Action::CommandChar(c),
            _ => return None,
        };
        Some(Command { count: 1, action })
    }
}
