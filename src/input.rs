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
use crate::picker::PickerKind;
use crate::registers::Sink;

#[derive(Default)]
pub struct Input {
    count: Option<usize>,
    /// The count typed *after* an operator — the `3` of `d3w`.
    motion_count: Option<usize>,
    operator: Option<Operator>,
    g_pending: bool,
    /// `"` has been typed and is waiting for the register it names.
    quote_pending: bool,
    /// Where this command's text goes. Reset with everything else.
    sink: Sink,
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
            Mode::Pick => Self::pick(key),
        }
    }

    /// What's been typed but not yet resolved, for the status line.
    pub fn pending_display(&self) -> String {
        let mut s = String::new();
        if let Some(n) = self.count {
            s.push_str(&n.to_string());
        }
        if self.quote_pending {
            s.push('"');
        }
        if self.sink == Sink::BlackHole {
            s.push_str("\"_");
        }
        match self.operator {
            Some(Operator::Delete) => s.push('d'),
            Some(Operator::Change) => s.push('c'),
            Some(Operator::Yank) => s.push('y'),
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
        let sink = self.sink;
        self.reset();
        Some(match operator {
            Some(op) => {
                Command { count: 1, action: Action::Operate { op, motion, count, sink } }
            }
            None => Command { count, action: Action::Move(motion) },
        })
    }

    /// Resolves a motion under an operator the key implies rather than one the
    /// user typed — `x` is `dl`, `Y` is `yy`.
    fn resolve_as(&mut self, op: Operator, motion: Motion) -> Option<Command> {
        self.operator = Some(op);
        self.resolve(motion)
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
        // `"` is holding out for the register it names. Only the black hole
        // exists so far; the picker and named registers are later steps.
        if self.quote_pending {
            self.quote_pending = false;
            if c == '_' {
                self.sink = Sink::BlackHole;
                return None;
            }
            // Nothing ever reaches the black hole, so nothing comes out of it.
            if (c == 'p' || c == 'P') && self.sink != Sink::BlackHole {
                self.reset();
                return Some(Command {
                    count: 1,
                    action: Action::OpenPicker(PickerKind::Register { before: c == 'P' }),
                });
            }
            self.reset();
            return None;
        }

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
                (Operator::Delete, 'd') | (Operator::Change, 'c') | (Operator::Yank, 'y')
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
            'u' => Action::Undo,
            // `x` is `dl` and always was — `Motion::Right` already stops at the
            // line end, so `5x` clamps there too.
            'x' => {
                return self.resolve_as(Operator::Delete, Motion::Right);
            }
            'p' | 'P' => {
                if self.sink == Sink::BlackHole {
                    // Nothing ever reaches the black hole, so nothing comes out.
                    self.reset();
                    return None;
                }
                let count = self.fold_count();
                self.reset();
                return Some(Command {
                    count: 1,
                    action: Action::Paste { before: c == 'P', count },
                });
            }
            // `Y` is `yy`, as in vim.
            'Y' => {
                return self.resolve_as(Operator::Yank, Motion::CurrentLine);
            }
            'd' => {
                self.operator = Some(Operator::Delete);
                return None;
            }
            'c' => {
                self.operator = Some(Operator::Change);
                return None;
            }
            'y' => {
                self.operator = Some(Operator::Yank);
                return None;
            }
            '"' => {
                self.quote_pending = true;
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

    fn pick(key: KeyEvent) -> Option<Command> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        let action = match key.code {
            KeyCode::Esc => Action::PickCancel,
            KeyCode::Char('c') if ctrl => Action::PickCancel,
            KeyCode::Char('n') if ctrl => Action::PickNext,
            KeyCode::Char('p') if ctrl => Action::PickPrev,
            KeyCode::Char('a') if ctrl => Action::PickToggleShort,
            KeyCode::Char(c) => Action::PickChar(c),
            KeyCode::Enter => Action::PickAccept,
            KeyCode::Backspace => Action::PickBackspace,
            KeyCode::Down => Action::PickNext,
            KeyCode::Up => Action::PickPrev,
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
    use crate::registers::Sink;
    use crate::picker::PickerKind;

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

    /// Like `typed`, but on an existing parser so leftover state shows up.
    fn typed_with(input: &mut Input, keys: &str) -> Command {
        let mut last = None;
        for c in keys.chars() {
            last = input.on_key(key(c), &Mode::Normal);
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
            Action::Operate { op: Operator::Delete, motion: Motion::WordForward, count: 1, sink: Sink::Ring }
        );
    }

    #[test]
    fn cw_carries_the_change_operator() {
        assert_eq!(
            typed("cw").action,
            Action::Operate { op: Operator::Change, motion: Motion::WordForward, count: 1, sink: Sink::Ring }
        );
    }

    #[test]
    fn the_doubled_form_is_the_current_line() {
        assert_eq!(
            typed("dd").action,
            Action::Operate { op: Operator::Delete, motion: Motion::CurrentLine, count: 1, sink: Sink::Ring }
        );
        assert_eq!(
            typed("cc").action,
            Action::Operate { op: Operator::Change, motion: Motion::CurrentLine, count: 1, sink: Sink::Ring }
        );
    }

    #[test]
    fn counts_on_both_sides_multiply() {
        assert_eq!(
            typed("2d3w").action,
            Action::Operate { op: Operator::Delete, motion: Motion::WordForward, count: 6, sink: Sink::Ring }
        );
    }

    #[test]
    fn a_count_after_the_operator_stands_alone() {
        assert_eq!(
            typed("d3w").action,
            Action::Operate { op: Operator::Delete, motion: Motion::WordForward, count: 3, sink: Sink::Ring }
        );
    }

    #[test]
    fn zero_after_an_operator_is_the_line_start_motion_not_a_count() {
        assert_eq!(
            typed("d0").action,
            Action::Operate { op: Operator::Delete, motion: Motion::LineStart, count: 1, sink: Sink::Ring }
        );
    }

    #[test]
    fn an_operator_reaches_through_the_g_prefix() {
        assert_eq!(
            typed("dgg").action,
            Action::Operate { op: Operator::Delete, motion: Motion::FirstLine, count: 1, sink: Sink::Ring }
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
            Action::Operate { op: Operator::Delete, motion: Motion::Line(5), count: 1, sink: Sink::Ring }
        );
    }

    #[test]
    fn yank_is_an_operator_like_the_others() {
        assert_eq!(
            typed("yw").action,
            Action::Operate {
                op: Operator::Yank,
                motion: Motion::WordForward,
                count: 1,
                sink: Sink::Ring
            }
        );
        assert_eq!(
            typed("yy").action,
            Action::Operate {
                op: Operator::Yank,
                motion: Motion::CurrentLine,
                count: 1,
                sink: Sink::Ring
            }
        );
    }

    /// `Y` is `yy`, and `x` is `dl`.
    #[test]
    fn the_shorthand_keys_expand_to_operators() {
        assert_eq!(
            typed("Y").action,
            Action::Operate {
                op: Operator::Yank,
                motion: Motion::CurrentLine,
                count: 1,
                sink: Sink::Ring
            }
        );
        assert_eq!(
            typed("x").action,
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::Right,
                count: 1,
                sink: Sink::Ring
            }
        );
        assert_eq!(
            typed("5x").action,
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::Right,
                count: 5,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn p_and_big_p_paste() {
        assert_eq!(typed("p").action, Action::Paste { before: false, count: 1 });
        assert_eq!(typed("P").action, Action::Paste { before: true, count: 1 });
        assert_eq!(typed("3p").action, Action::Paste { before: false, count: 3 });
    }

    /// The one register that exists so far. This must survive the reset that
    /// happens when the command resolves.
    #[test]
    fn the_black_hole_prefix_reaches_the_operator() {
        assert_eq!(
            typed("\"_dd").action,
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::CurrentLine,
                count: 1,
                sink: Sink::BlackHole
            }
        );
        assert_eq!(
            typed("\"_dw").action,
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::WordForward,
                count: 1,
                sink: Sink::BlackHole
            }
        );
    }

    #[test]
    fn the_black_hole_does_not_leak_into_the_next_command() {
        let mut input = Input::default();
        for c in "\"_dd".chars() {
            input.on_key(key(c), &Mode::Normal);
        }
        assert_eq!(
            typed_with(&mut input, "dd").action,
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::CurrentLine,
                count: 1,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn nothing_comes_back_out_of_the_black_hole() {
        assert!(nothing("\"_p").is_none());
    }

    /// An unknown register name discards the command. Keys after it are fresh
    /// input, so `"zdd` deletes to the ring — the `"z` is dropped, not the `dd`.
    #[test]
    fn quote_p_opens_the_picker() {
        assert_eq!(
            typed("\"p").action,
            Action::OpenPicker(PickerKind::Register { before: false })
        );
        assert_eq!(
            typed("\"P").action,
            Action::OpenPicker(PickerKind::Register { before: true })
        );
    }

    #[test]
    fn picker_keys_map_to_pick_actions() {
        let mut input = Input::default();
        let mut act = |k: KeyEvent| input.on_key(k, &Mode::Pick).unwrap().action;

        assert_eq!(act(key('a')), Action::PickChar('a'));
        assert_eq!(act(ctrl('n')), Action::PickNext);
        assert_eq!(act(ctrl('p')), Action::PickPrev);
        assert_eq!(act(ctrl('a')), Action::PickToggleShort);
        assert_eq!(act(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Action::PickAccept);
        assert_eq!(act(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), Action::PickCancel);
        assert_eq!(
            act(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            Action::PickBackspace
        );
        assert_eq!(act(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)), Action::PickNext);
        assert_eq!(act(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)), Action::PickPrev);
    }

    /// `p` is a literal in the picker's query, not the paste key.
    #[test]
    fn a_plain_p_in_the_picker_is_a_query_char() {
        let mut input = Input::default();
        assert_eq!(
            input.on_key(key('p'), &Mode::Pick).unwrap().action,
            Action::PickChar('p')
        );
    }

    #[test]
    fn a_quote_naming_no_register_cancels() {
        assert!(nothing("\"z").is_none());
        assert_eq!(
            typed_with(&mut Input::default(), "\"zdd").action,
            Action::Operate {
                op: Operator::Delete,
                motion: Motion::CurrentLine,
                count: 1,
                sink: Sink::Ring
            }
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
