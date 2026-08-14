//! Key events → [`Command`].
//!
//! The keymap is hardcoded on purpose. Extracting it into config is a step-2
//! problem; doing it now would make the config format the project.
//!
//! Normal mode is a small state machine rather than a lookup, because Vim's
//! grammar is `[count] operator [count] motion` and every part is optional. The
//! state is what has been typed but not yet resolved: a count, an operator
//! waiting for its motion, a second count belonging to that motion, and whether
//! `g` is holding out for its second key.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::editor::{Action, Command, Mode};
use crate::motion::{Motion, Operator};

#[derive(Default)]
pub struct Input {
    count: Option<usize>,
    /// The count typed *after* an operator — the `3` of `d3w`.
    motion_count: Option<usize>,
    operator: Option<Operator>,
    g_pending: bool,
}

/// The keys that name a motion on their own. `G` is missing because what it
/// means depends on whether a count was typed.
fn motion_key(c: char) -> Option<Motion> {
    Some(match c {
        'h' => Motion::Left,
        'l' | ' ' => Motion::Right,
        'j' => Motion::Down,
        'k' => Motion::Up,
        'w' => Motion::WordForward,
        'b' => Motion::WordBackward,
        '0' | '^' => Motion::LineStart,
        '$' => Motion::LineEnd,
        _ => return None,
    })
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
        match self.operator {
            Some(Operator::Delete) => s.push('d'),
            Some(Operator::Change) => s.push('c'),
            None => {}
        }
        if let Some(n) = self.motion_count {
            s.push_str(&n.to_string());
        }
        if self.g_pending {
            s.push('g');
        }
        s
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Counts multiply, so `2d3w` covers six words.
    fn fold_count(&self) -> usize {
        self.count.unwrap_or(1).max(1) * self.motion_count.unwrap_or(1).max(1)
    }

    /// The count as the user typed it, if they typed one — what `G` needs to
    /// tell "last line" from "line 5".
    fn explicit_count(&self) -> Option<usize> {
        self.motion_count.or(self.count)
    }

    /// Resolves a motion: applies the pending operator to it, or just moves.
    fn resolve(&mut self, motion: Motion) -> Option<Command> {
        // An absolute motion already spent the count naming its destination —
        // `d5G` deletes to line 5 once, not five times.
        let count = if motion.is_absolute() { 1 } else { self.fold_count() };
        let operator = self.operator;
        self.reset();
        Some(match operator {
            Some(op) => Command { count: 1, action: Action::Operate { op, motion, count } },
            None => Command { count, action: Action::Move(motion) },
        })
    }

    /// A plain action, with whatever count preceded it.
    fn plain(&mut self, action: Action) -> Option<Command> {
        let count = self.fold_count();
        self.reset();
        Some(Command { count, action })
    }

    fn normal(&mut self, key: KeyEvent) -> Option<Command> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => {
                self.reset();
                None
            }
            KeyCode::Char('c') if ctrl => {
                self.reset();
                None
            }
            KeyCode::Char('r') if ctrl => self.plain(Action::Redo),
            KeyCode::Char(c) => self.normal_char(c),
            KeyCode::Left => self.resolve(Motion::Left),
            KeyCode::Right => self.resolve(Motion::Right),
            KeyCode::Down => self.resolve(Motion::Down),
            KeyCode::Up => self.resolve(Motion::Up),
            KeyCode::Home => self.resolve(Motion::LineStart),
            KeyCode::End => self.resolve(Motion::LineEnd),
            _ => {
                self.reset();
                None
            }
        }
    }

    fn normal_char(&mut self, c: char) -> Option<Command> {
        // `g` is holding out for a second key.
        if self.g_pending {
            self.g_pending = false;
            return match c {
                'g' => self.resolve(Motion::FirstLine),
                _ => {
                    self.reset();
                    None
                }
            };
        }

        // Digits build a count — except a leading `0`, which is a motion. After
        // an operator the digits belong to the motion, not to the operator.
        let slot = if self.operator.is_some() { self.motion_count } else { self.count };
        if c.is_ascii_digit() && !(c == '0' && slot.is_none()) {
            let n = slot.unwrap_or(0) * 10 + c.to_digit(10).unwrap() as usize;
            if self.operator.is_some() {
                self.motion_count = Some(n);
            } else {
                self.count = Some(n);
            }
            return None;
        }

        // An operator is waiting: this key is its motion, its doubled form, or
        // nothing at all — in which case the operator is abandoned.
        if let Some(op) = self.operator {
            let doubled = matches!(
                (op, c),
                (Operator::Delete, 'd') | (Operator::Change, 'c')
            );
            if doubled {
                return self.resolve(Motion::CurrentLine);
            }
            if c == 'g' {
                self.g_pending = true;
                return None;
            }
            if c == 'G' {
                let m = match self.explicit_count() {
                    Some(n) => Motion::Line(n),
                    None => Motion::LastLine,
                };
                return self.resolve(m);
            }
            return match motion_key(c) {
                Some(m) => self.resolve(m),
                None => {
                    self.reset();
                    None
                }
            };
        }

        if let Some(m) = motion_key(c) {
            return self.resolve(m);
        }

        let action = match c {
            'i' => Action::EnterInsert,
            'a' => Action::EnterInsertAfter,
            'I' => Action::EnterInsertLineStart,
            'A' => Action::EnterInsertLineEnd,
            'o' => Action::OpenLineBelow,
            'O' => Action::OpenLineAbove,
            'x' => Action::DeleteChar,
            'u' => Action::Undo,
            'd' => {
                self.operator = Some(Operator::Delete);
                return None;
            }
            'c' => {
                self.operator = Some(Operator::Change);
                return None;
            }
            'g' => {
                self.g_pending = true;
                return None;
            }
            // `G` with a count is "go to line N", without one it's "go to the
            // end" — so the count isn't a repeat here.
            'G' => {
                let m = match self.explicit_count() {
                    Some(n) => Motion::Line(n),
                    None => Motion::LastLine,
                };
                return self.resolve(m);
            }
            ':' => {
                self.reset();
                return Some(Command { count: 1, action: Action::EnterCommandMode });
            }
            _ => {
                self.reset();
                return None;
            }
        };
        self.plain(action)
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
            KeyCode::Left => Action::Move(Motion::Left),
            KeyCode::Right => Action::Move(Motion::Right),
            KeyCode::Down => Action::Move(Motion::Down),
            KeyCode::Up => Action::Move(Motion::Up),
            KeyCode::Home => Action::Move(Motion::LineStart),
            KeyCode::End => Action::Move(Motion::LineEnd),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Feeds `keys` and returns the one command they produce, asserting that
    /// every key before the last resolved to nothing.
    fn typed(keys: &str) -> Command {
        let mut input = Input::default();
        let mut last = None;
        for (i, c) in keys.chars().enumerate() {
            let out = input.on_key(key(c), &Mode::Normal);
            if i + 1 < keys.chars().count() {
                assert!(out.is_none(), "{c:?} resolved early in {keys:?}");
            }
            last = out;
        }
        last.unwrap_or_else(|| panic!("{keys:?} produced no command"))
    }

    fn nothing(keys: &str) -> Option<Command> {
        let mut input = Input::default();
        let mut last = None;
        for c in keys.chars() {
            last = input.on_key(key(c), &Mode::Normal);
        }
        last
    }

    #[test]
    fn a_bare_motion_moves() {
        let cmd = typed("w");
        assert_eq!(cmd.action, Action::Move(Motion::WordForward));
        assert_eq!(cmd.count, 1);
    }

    #[test]
    fn a_count_before_a_motion_repeats_it() {
        let cmd = typed("12j");
        assert_eq!(cmd.action, Action::Move(Motion::Down));
        assert_eq!(cmd.count, 12);
    }

    #[test]
    fn dw_is_an_operator_over_a_motion() {
        assert_eq!(
            typed("dw").action,
            Action::Operate { op: Operator::Delete, motion: Motion::WordForward, count: 1 }
        );
    }

    #[test]
    fn cw_carries_the_change_operator() {
        assert_eq!(
            typed("cw").action,
            Action::Operate { op: Operator::Change, motion: Motion::WordForward, count: 1 }
        );
    }

    #[test]
    fn the_doubled_form_is_the_current_line() {
        assert_eq!(
            typed("dd").action,
            Action::Operate { op: Operator::Delete, motion: Motion::CurrentLine, count: 1 }
        );
        assert_eq!(
            typed("cc").action,
            Action::Operate { op: Operator::Change, motion: Motion::CurrentLine, count: 1 }
        );
    }

    #[test]
    fn counts_on_both_sides_multiply() {
        assert_eq!(
            typed("2d3w").action,
            Action::Operate { op: Operator::Delete, motion: Motion::WordForward, count: 6 }
        );
    }

    #[test]
    fn a_count_after_the_operator_stands_alone() {
        assert_eq!(
            typed("d3w").action,
            Action::Operate { op: Operator::Delete, motion: Motion::WordForward, count: 3 }
        );
    }

    #[test]
    fn zero_after_an_operator_is_the_line_start_motion_not_a_count() {
        assert_eq!(
            typed("d0").action,
            Action::Operate { op: Operator::Delete, motion: Motion::LineStart, count: 1 }
        );
    }

    #[test]
    fn an_operator_reaches_through_the_g_prefix() {
        assert_eq!(
            typed("dgg").action,
            Action::Operate { op: Operator::Delete, motion: Motion::FirstLine, count: 1 }
        );
    }

    #[test]
    fn bare_gg_still_just_moves() {
        assert_eq!(typed("gg").action, Action::Move(Motion::FirstLine));
    }

    #[test]
    fn g_with_a_count_names_a_line_and_without_one_the_last() {
        assert_eq!(typed("G").action, Action::Move(Motion::LastLine));
        assert_eq!(typed("5G").action, Action::Move(Motion::Line(5)));
        assert_eq!(
            typed("d5G").action,
            Action::Operate { op: Operator::Delete, motion: Motion::Line(5), count: 1 }
        );
    }

    #[test]
    fn an_operator_followed_by_a_non_motion_is_abandoned() {
        assert!(nothing("dz").is_none(), "z is not a motion, so dz does nothing");
        assert!(nothing("di").is_none(), "and the operator does not leak into insert");
    }

    #[test]
    fn escape_clears_a_half_typed_command() {
        let mut input = Input::default();
        input.on_key(key('2'), &Mode::Normal);
        input.on_key(key('d'), &Mode::Normal);
        assert_eq!(input.pending_display(), "2d");

        input.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &Mode::Normal);
        assert_eq!(input.pending_display(), "");
    }

    #[test]
    fn the_pending_display_shows_the_whole_half_typed_command() {
        let mut input = Input::default();
        for c in "2d3".chars() {
            input.on_key(key(c), &Mode::Normal);
        }
        assert_eq!(input.pending_display(), "2d3");
        input.on_key(key('g'), &Mode::Normal);
        assert_eq!(input.pending_display(), "2d3g");
    }

    #[test]
    fn u_undoes_and_ctrl_r_redoes() {
        let mut input = Input::default();
        assert_eq!(input.on_key(key('u'), &Mode::Normal).unwrap().action, Action::Undo);
        assert_eq!(input.on_key(ctrl('r'), &Mode::Normal).unwrap().action, Action::Redo);
    }

    #[test]
    fn undo_and_redo_take_a_count() {
        let mut input = Input::default();
        assert_eq!(typed("3u").count, 3);

        assert!(input.on_key(key('2'), &Mode::Normal).is_none());
        let cmd = input.on_key(ctrl('r'), &Mode::Normal).unwrap();
        assert_eq!(cmd.count, 2);
        assert_eq!(cmd.action, Action::Redo);
    }

    /// `u`, `d` and `c` are normal-mode keys, not text.
    #[test]
    fn operator_keys_are_just_letters_in_insert_mode() {
        let mut input = Input::default();
        for c in ['u', 'd', 'c'] {
            let cmd = input.on_key(key(c), &Mode::Insert).unwrap();
            assert_eq!(cmd.action, Action::InsertChar(c));
        }
    }
}
