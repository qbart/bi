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

use crate::editor::{Action, BufferCmd, Command, Mode, TreeCmd, VisualKind, WindowCmd};
use crate::key::{Key, KeyCode};
use crate::motion::{Motion, Operator, Target, TextObject};
use crate::picker::PickerKind;
use crate::registers::Sink;
use crate::window::{ContentKind, Dir, Side};

#[derive(Default)]
pub struct Input {
    count: Option<usize>,
    /// The count typed *after* an operator — the `3` of `d3w`.
    motion_count: Option<usize>,
    operator: Option<Operator>,
    g_pending: bool,
    /// `"` has been typed and is waiting for the register it names.
    quote_pending: bool,
    /// `r` has been typed and is waiting for the character to write.
    replace_pending: bool,
    /// `f`/`F`/`t`/`T` has been typed and is waiting for its target character.
    find_pending: Option<(bool, bool)>,
    /// `i` or `a` has been typed under an operator and is waiting for the
    /// object it selects. The bool is the `a` of `aw`.
    object_pending: Option<bool>,
    /// `Ctrl-W` has been typed and is waiting for the key that says what to do
    /// with a window. Not a key on its own — the start of one.
    window_pending: bool,
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

/// `(forward, till)` for the four find keys.
fn find_key(c: char) -> Option<(bool, bool)> {
    Some(match c {
        'f' => (true, false),
        't' => (true, true),
        'F' => (false, false),
        'T' => (false, true),
        _ => return None,
    })
}

/// The key that names a text object, after `i` or `a`.
///
/// `b` and `B` are vim's aliases for `(` and `{`, and cost nothing here.
fn object_key(c: char) -> Option<TextObject> {
    Some(match c {
        'w' => TextObject::Word { big: false },
        'W' => TextObject::Word { big: true },
        'p' => TextObject::Paragraph,
        '"' | '\'' | '`' => TextObject::Quoted(c),
        '(' | ')' | 'b' => TextObject::Delimited('('),
        '[' | ']' => TextObject::Delimited('['),
        '{' | '}' | 'B' => TextObject::Delimited('{'),
        '<' | '>' => TextObject::Delimited('<'),
        _ => return None,
    })
}

impl Input {
    pub fn on_key(&mut self, key: Key, mode: &Mode, content: ContentKind) -> Option<Command> {
        match mode {
            // A tree gets its own keymap rather than an overlay on normal
            // mode's. Which one runs is a property of the window, not of the
            // session — see `docs/specs/tree.md` on why `Mode::Tree` would be
            // a second copy of a fact the window already holds.
            Mode::Normal if content == ContentKind::Tree => self.tree(key),
            Mode::Normal => self.normal(key),
            // Visual shares normal's grammar: the same motions, counts and
            // text objects, differing only in what an operator applies to.
            Mode::Visual(kind) => self.visual(key, *kind),
            Mode::Insert => Self::insert(key),
            Mode::Replace => Self::replace(key),
            Mode::Command(_) => Self::command_line(key),
            Mode::Search { .. } => Self::search_line(key),
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
        if self.replace_pending {
            s.push('r');
        }
        if let Some((forward, till)) = self.find_pending {
            s.push(match (forward, till) {
                (true, false) => 'f',
                (true, true) => 't',
                (false, false) => 'F',
                (false, true) => 'T',
            });
        }
        if let Some(around) = self.object_pending {
            s.push(if around { 'a' } else { 'i' });
        }
        if self.window_pending {
            s.push_str("^W");
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
            Some(op) => Command {
                count: 1,
                action: Action::Operate { op, target: Target::Motion(motion), count, sink },
            },
            None => Command { count, action: Action::Move(motion) },
        })
    }

    /// Resolves a text object. Unlike a motion it is only ever a target, so
    /// there is no "just move there" case — `iw` on its own does nothing until
    /// visual mode gives it one.
    fn resolve_object(&mut self, object: TextObject, around: bool) -> Option<Command> {
        let op = self.operator?;
        let count = self.fold_count();
        let sink = self.sink;
        self.reset();
        Some(Command {
            count: 1,
            action: Action::Operate { op, target: Target::Object { object, around }, count, sink },
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

    /// Resolves the key after `Ctrl-W`.
    ///
    /// A count in front belongs to the resize keys, where `3 Ctrl-W +` is three
    /// rows; every other window key ignores it, which is what vim does and the
    /// only place a count means anything here.
    fn window_key(&mut self, key: Key) -> Option<Command> {
        let cells = self.fold_count() as i32;
        let cmd = match key.code {
            // Ctrl or not: `Ctrl-W Ctrl-W` cycles just as `Ctrl-W w` does, so
            // the finger already holding ctrl does not have to let go.
            KeyCode::Char('w') => WindowCmd::Cycle { back: false },
            KeyCode::Char('W') => WindowCmd::Cycle { back: true },
            KeyCode::Char('s') | KeyCode::Char('S') => {
                WindowCmd::Split { dir: Dir::Horizontal, path: None }
            }
            KeyCode::Char('v') => WindowCmd::Split { dir: Dir::Vertical, path: None },
            KeyCode::Char('h') => WindowCmd::Focus(Side::Left),
            KeyCode::Char('j') => WindowCmd::Focus(Side::Down),
            KeyCode::Char('k') => WindowCmd::Focus(Side::Up),
            KeyCode::Char('l') => WindowCmd::Focus(Side::Right),
            KeyCode::Left => WindowCmd::Focus(Side::Left),
            KeyCode::Down => WindowCmd::Focus(Side::Down),
            KeyCode::Up => WindowCmd::Focus(Side::Up),
            KeyCode::Right => WindowCmd::Focus(Side::Right),
            // `q` quits the window, which is closing it.
            KeyCode::Char('c') | KeyCode::Char('q') => WindowCmd::Close,
            KeyCode::Char('o') => WindowCmd::Only,
            KeyCode::Char('+') => WindowCmd::Resize { axis: Dir::Horizontal, cells },
            KeyCode::Char('-') => WindowCmd::Resize { axis: Dir::Horizontal, cells: -cells },
            KeyCode::Char('>') => WindowCmd::Resize { axis: Dir::Vertical, cells },
            KeyCode::Char('<') => WindowCmd::Resize { axis: Dir::Vertical, cells: -cells },
            KeyCode::Char('=') => WindowCmd::Equalize,
            // Esc cancels, and anything unrecognised drops the prefix rather
            // than swallowing the key — the same rule the rest of this file
            // follows.
            _ => {
                self.reset();
                return None;
            }
        };
        self.reset();
        Some(Command { count: 1, action: Action::Window(cmd) })
    }

    /// The keymap for a window holding a tree.
    ///
    /// Complete, not an overlay: nothing falls through to normal mode except
    /// what is named here. An allowlist stays correct as keys are added to the
    /// editor, where a denylist — "normal mode, minus the ones that edit" —
    /// would have to be revisited every time and would be silently wrong until
    /// someone noticed. For a pane sitting on a filesystem that is worth the
    /// keys it leaves out.
    ///
    /// Nothing here enters insert or visual mode, and `Ctrl-W` is normal-mode
    /// only, so a tree can never be focused in either.
    fn tree(&mut self, key: Key) -> Option<Command> {
        if self.window_pending {
            return self.window_key(key);
        }
        let ctrl = key.mods.ctrl;
        let count = self.count.unwrap_or(1).max(1);
        let g = std::mem::take(&mut self.g_pending);

        let tree_cmd = match key.code {
            // The `g` prefix resolves first, or `gh` would read as `h`.
            KeyCode::Char('g') if g => TreeCmd::First,
            KeyCode::Char('h') if g => TreeCmd::ToggleHidden,
            KeyCode::Char('g') => {
                self.g_pending = true;
                return None;
            }

            KeyCode::Char(c) if c.is_ascii_digit() && !(c == '0' && self.count.is_none()) => {
                self.count = Some(self.count.unwrap_or(0) * 10 + c.to_digit(10).unwrap() as usize);
                return None;
            }
            KeyCode::Char('w') if ctrl => {
                self.window_pending = true;
                return None;
            }
            KeyCode::Char(':') => return self.plain(Action::EnterCommandMode),
            KeyCode::Char('^') if ctrl => return self.plain(Action::Buffer(BufferCmd::Alternate)),

            KeyCode::Char('j') | KeyCode::Down => TreeCmd::Select { down: true, count },
            KeyCode::Char('k') | KeyCode::Up => TreeCmd::Select { down: false, count },
            KeyCode::Char('d') if ctrl => TreeCmd::HalfPage { down: true },
            KeyCode::Char('u') if ctrl => TreeCmd::HalfPage { down: false },
            KeyCode::Char('G') => TreeCmd::Last,
            KeyCode::Char('l') | KeyCode::Right => TreeCmd::Expand,
            KeyCode::Char('h') | KeyCode::Left => TreeCmd::Collapse,
            KeyCode::Enter => TreeCmd::Enter,
            KeyCode::Char('-') => TreeCmd::Up,
            KeyCode::Char('R') => TreeCmd::Refresh,

            // Everything else, Esc included, drops what was pending and does
            // nothing — which is what an allowlist means.
            _ => {
                self.reset();
                return None;
            }
        };
        self.plain(Action::Tree(tree_cmd))
    }

    fn normal(&mut self, key: Key) -> Option<Command> {
        let ctrl = key.mods.ctrl;

        // Checked before everything else: while the prefix is armed, `s` means
        // split rather than substitute and `l` means "the window to the right"
        // rather than a motion.
        if self.window_pending {
            return self.window_key(key);
        }

        match key.code {
            // Esc clears the pending keymap state *and* drops any extra
            // cursors. Collapsing with one cursor is a no-op, so this can be
            // unconditional.
            KeyCode::Esc => {
                self.reset();
                Some(Command { count: 1, action: Action::CollapseCursors })
            }
            KeyCode::Char('c') if ctrl => {
                self.reset();
                Some(Command { count: 1, action: Action::CollapseCursors })
            }
            KeyCode::Char('r') if ctrl => self.plain(Action::Redo),
            KeyCode::Char('n') if ctrl => self.plain(Action::AddCursorNextMatch),
            KeyCode::Char('v') if ctrl => self.plain(Action::EnterVisual(VisualKind::Block)),
            KeyCode::Char('e') if ctrl => self.plain(Action::ScrollLine { down: true }),
            KeyCode::Char('y') if ctrl => self.plain(Action::ScrollLine { down: false }),
            KeyCode::Char('d') if ctrl => self.plain(Action::ScrollHalfPage { down: true }),
            KeyCode::Char('u') if ctrl => self.plain(Action::ScrollHalfPage { down: false }),
            // Not every terminal sends this one, which is why `:b#` exists
            // beside it rather than only underneath it.
            KeyCode::Char('^') if ctrl => self.plain(Action::Buffer(BufferCmd::Alternate)),
            // The tree on this file's directory. In a tree the same key goes
            // up a level, which is the same move one step further out.
            KeyCode::Char('-') => self.plain(Action::Tree(TreeCmd::Up)),
            // The start of a key, not a key. Any count already typed stays,
            // because it belongs to the resize forms.
            KeyCode::Char('w') if ctrl => {
                self.window_pending = true;
                None
            }
            KeyCode::Down if ctrl && key.mods.alt => {
                self.plain(Action::AddCursorLine { below: true })
            }
            KeyCode::Up if ctrl && key.mods.alt => {
                self.plain(Action::AddCursorLine { below: false })
            }
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
        // `r` holds out for the character to write. Checked before everything
        // else, so `r5` writes a `5` rather than starting a count and `rd`
        // writes a `d` rather than starting an operator.
        if self.replace_pending {
            let count = self.fold_count();
            self.reset();
            return Some(Command { count: 1, action: Action::ReplaceChar { ch: c, count } });
        }

        // `f`/`t` and friends hold out for their target, which is taken
        // literally for the same reason `r`'s is.
        if let Some((forward, till)) = self.find_pending.take() {
            return self.resolve(Motion::FindChar { ch: c, forward, till, repeat: false });
        }

        // `i`/`a` under an operator hold out for the object they select.
        if let Some(around) = self.object_pending.take() {
            return match object_key(c) {
                Some(object) => self.resolve_object(object, around),
                None => {
                    self.reset();
                    None
                }
            };
        }

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
            if c == 'i' || c == 'a' {
                self.object_pending = Some(c == 'a');
                return None;
            }
            if let Some(pending) = find_key(c) {
                self.find_pending = Some(pending);
                return None;
            }
            if c == ';' || c == ',' {
                return self.resolve(Motion::RepeatFind { reverse: c == ',' });
            }
            // `d/foo<CR>` and `dn`. The search line is a mode of its own, so
            // the operator has to travel with the mode change rather than
            // waiting here — `reset()` runs on the way in.
            if c == '/' || c == '?' {
                let operator = Some((op, self.sink));
                let count = self.fold_count();
                self.reset();
                return Some(Command {
                    count: 1,
                    action: Action::EnterSearch { forward: c == '/', operator, count },
                });
            }
            if c == 'n' || c == 'N' {
                return self.resolve(Motion::Search { reverse: c == 'N' });
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
            // The operator shorthands. Same trick as `x` and `Y`: an operator
            // the key implies rather than one the user typed.
            'D' => return self.resolve_as(Operator::Delete, Motion::LineEnd),
            'C' => return self.resolve_as(Operator::Change, Motion::LineEnd),
            'S' => return self.resolve_as(Operator::Change, Motion::CurrentLine),
            's' => return self.resolve_as(Operator::Change, Motion::Right),
            'X' => return self.resolve_as(Operator::Delete, Motion::Left),
            'r' => {
                self.replace_pending = true;
                return None;
            }
            '/' | '?' => {
                // The pending operator travels with the mode change: entering
                // the search line resets the keymap, so `d/foo` would lose it.
                let operator = self.operator.map(|op| (op, self.sink));
                let count = self.fold_count();
                self.reset();
                return Some(Command {
                    count: 1,
                    action: Action::EnterSearch { forward: c == '/', operator, count },
                });
            }
            'n' => return self.resolve(Motion::Search { reverse: false }),
            'N' => return self.resolve(Motion::Search { reverse: true }),
            '*' => return self.plain(Action::SearchWord { forward: true }),
            '#' => return self.plain(Action::SearchWord { forward: false }),
            'v' => return self.plain(Action::EnterVisual(VisualKind::Char)),
            'V' => return self.plain(Action::EnterVisual(VisualKind::Line)),
            'R' => return self.plain(Action::EnterReplace),
            'f' | 'F' | 't' | 'T' => {
                self.find_pending = find_key(c);
                return None;
            }
            ';' | ',' => return self.resolve(Motion::RepeatFind { reverse: c == ',' }),
            '.' => {
                let count = self.explicit_count();
                self.reset();
                return Some(Command { count: 1, action: Action::RepeatChange { count } });
            }
            '~' => {
                let count = self.fold_count();
                self.reset();
                return Some(Command { count: 1, action: Action::ToggleCase { count } });
            }
            'J' => {
                let count = self.fold_count();
                self.reset();
                return Some(Command { count: 1, action: Action::JoinLines { count } });
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

    /// Visual mode. Falls through to `normal` for everything it does not
    /// claim, so every motion and text object works unchanged.
    fn visual(&mut self, key: Key, kind: VisualKind) -> Option<Command> {
        let ctrl = key.mods.ctrl;
        let block = kind == VisualKind::Block;

        // Esc has to be claimed here. Normal mode's Esc only clears the pending
        // keymap state and resolves to no command at all, which would leave
        // visual mode running with the user believing they had left it.
        if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
            self.reset();
            return Some(Command { count: 1, action: Action::EnterNormal });
        }
        // `Ctrl-N` in visual selects the next occurrence of the selection,
        // which is the multi-cursor idiom people expect from other editors.
        // Not in a block: the rectangle is derived from one selection's
        // corners, so a second one has nothing to say.
        if ctrl && key.code == KeyCode::Char('n') && !block {
            return self.plain(Action::AddCursorNextMatch);
        }

        let KeyCode::Char(c) = key.code else {
            return self.normal(key);
        };
        if ctrl {
            return self.normal(key);
        }

        // `r` over a selection overwrites every character in it, so it does not
        // fall through to normal mode's one-character form.
        if self.replace_pending {
            self.reset();
            return Some(Command { count: 1, action: Action::ReplaceSelection(c) });
        }

        // `i`/`a` name a text object here rather than entering insert mode, and
        // the object becomes the selection rather than being operated on. This
        // is what makes `viw` and `vi(` work.
        if let Some(around) = self.object_pending.take() {
            return match object_key(c) {
                Some(object) => {
                    self.reset();
                    Some(Command { count: 1, action: Action::SelectObject { object, around } })
                }
                None => {
                    self.reset();
                    None
                }
            };
        }
        if c == 'i' || c == 'a' {
            self.object_pending = Some(c == 'a');
            return None;
        }

        // An operator in visual mode takes the selection, not a motion, so it
        // resolves immediately rather than waiting for one.
        let op = match c {
            'd' | 'x' => Some(Operator::Delete),
            'c' | 's' => Some(Operator::Change),
            'y' => Some(Operator::Yank),
            _ => None,
        };
        if let Some(op) = op {
            let sink = self.sink;
            self.reset();
            return Some(Command { count: 1, action: Action::OperateSelection { op, sink } });
        }

        match c {
            'o' => {
                self.reset();
                Some(Command { count: 1, action: Action::SwapEnds })
            }
            // Vim's `O` is `o` outside a block, and the horizontal flip inside
            // one.
            'O' => {
                self.reset();
                let action = if block { Action::SwapCorners } else { Action::SwapEnds };
                Some(Command { count: 1, action })
            }
            // Only blockwise gives `I`/`A` a meaning of their own; elsewhere
            // they are normal mode's, which is what vim does too.
            'I' if block => self.plain(Action::BlockInsert { append: false }),
            'A' if block => self.plain(Action::BlockInsert { append: true }),
            'v' => self.plain(Action::EnterVisual(VisualKind::Char)),
            'V' => self.plain(Action::EnterVisual(VisualKind::Line)),
            _ => self.normal(key),
        }
    }

    /// Replace mode. Printable keys overwrite; `Backspace` puts back what was
    /// overwritten rather than deleting.
    fn replace(key: Key) -> Option<Command> {
        let ctrl = key.mods.ctrl;
        let action = match key.code {
            KeyCode::Esc => Action::EnterNormal,
            KeyCode::Char('c') if ctrl => Action::EnterNormal,
            KeyCode::Char(c) => Action::ReplaceTyped(c),
            KeyCode::Backspace => Action::ReplaceBackspace,
            KeyCode::Enter => Action::InsertNewline,
            KeyCode::Tab => Action::ReplaceTyped('\t'),
            KeyCode::Left => Action::Move(Motion::Left),
            KeyCode::Right => Action::Move(Motion::Right),
            KeyCode::Down => Action::Move(Motion::Down),
            KeyCode::Up => Action::Move(Motion::Up),
            KeyCode::Home => Action::Move(Motion::LineStart),
            KeyCode::End => Action::Move(Motion::LineEnd),
        };
        Some(Command { count: 1, action })
    }

    fn insert(key: Key) -> Option<Command> {
        let ctrl = key.mods.ctrl;

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
        };
        Some(Command { count: 1, action })
    }

    fn pick(key: Key) -> Option<Command> {
        let ctrl = key.mods.ctrl;

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

    /// The `/` or `?` line. Same shape as the `:` line — every printable key
    /// is pattern text, so nothing here can be a command.
    fn search_line(key: Key) -> Option<Command> {
        let ctrl = key.mods.ctrl;
        let action = match key.code {
            KeyCode::Esc => Action::SearchCancel,
            KeyCode::Char('c') if ctrl => Action::SearchCancel,
            KeyCode::Enter => Action::SearchExecute,
            KeyCode::Backspace => Action::SearchBackspace,
            KeyCode::Char(c) => Action::SearchChar(c),
            _ => return None,
        };
        Some(Command { count: 1, action })
    }

    fn command_line(key: Key) -> Option<Command> {
        let ctrl = key.mods.ctrl;

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
    use crate::picker::PickerKind;
    use crate::registers::Sink;

    fn key(c: char) -> Key {
        Key::char(c)
    }

    fn ctrl(c: char) -> Key {
        Key::ctrl(c)
    }

    /// Feeds `keys` to a window holding a tree, returning what the last one
    /// resolved to.
    fn in_tree(keys: &str) -> Option<Command> {
        let mut input = Input::default();
        let mut last = None;
        for c in keys.chars() {
            last = input.on_key(key(c), &Mode::Normal, ContentKind::Tree);
        }
        last
    }

    fn tree_action(keys: &str) -> Action {
        in_tree(keys).unwrap_or_else(|| panic!("{keys:?} produced no command")).action
    }

    #[test]
    fn minus_asks_for_the_tree_from_either_side() {
        assert_eq!(typed("-").action, Action::Tree(TreeCmd::Up), "from a file");
        assert_eq!(tree_action("-"), Action::Tree(TreeCmd::Up), "and from a tree");
    }

    #[test]
    fn the_tree_keymap_moves_by_a_count() {
        assert_eq!(tree_action("j"), Action::Tree(TreeCmd::Select { down: true, count: 1 }));
        assert_eq!(tree_action("3j"), Action::Tree(TreeCmd::Select { down: true, count: 3 }));
        assert_eq!(tree_action("k"), Action::Tree(TreeCmd::Select { down: false, count: 1 }));
    }

    #[test]
    fn the_tree_keymap_expands_collapses_and_re_roots() {
        assert_eq!(tree_action("l"), Action::Tree(TreeCmd::Expand));
        assert_eq!(tree_action("h"), Action::Tree(TreeCmd::Collapse));
        assert_eq!(tree_action("-"), Action::Tree(TreeCmd::Up));
        assert_eq!(tree_action("R"), Action::Tree(TreeCmd::Refresh));
    }

    /// `gh` sits beside `gg` under the prefix normal mode already runs, and
    /// the prefix has to resolve before `h` reads as collapse.
    #[test]
    fn the_g_prefix_tells_first_row_from_hidden_files() {
        assert_eq!(tree_action("gg"), Action::Tree(TreeCmd::First));
        assert_eq!(tree_action("gh"), Action::Tree(TreeCmd::ToggleHidden));
        assert_eq!(tree_action("G"), Action::Tree(TreeCmd::Last));
    }

    /// The keymap is an allowlist, and this is what that buys: no key that
    /// edits or enters insert mode can reach a pane sitting on a filesystem.
    #[test]
    fn nothing_in_a_tree_enters_insert_or_edits() {
        for c in ['i', 'a', 'A', 'I', 'o', 'O', 'c', 'x', 'p', 'v', 'V', 'd', 'u', 's'] {
            assert!(in_tree(&c.to_string()).is_none(), "{c:?} did something in a tree");
        }
    }

    /// What it does let through: the window prefix and the command line, so a
    /// tree pane is still a window and `:` still works from one.
    #[test]
    fn a_tree_still_takes_window_keys_and_the_command_line() {
        let mut input = Input::default();
        assert!(input.on_key(ctrl('w'), &Mode::Normal, ContentKind::Tree).is_none(), "armed");
        let cmd = input.on_key(key('v'), &Mode::Normal, ContentKind::Tree).expect("resolved");
        assert_eq!(cmd.action, Action::Window(WindowCmd::Split { dir: Dir::Vertical, path: None }));

        assert_eq!(tree_action(":"), Action::EnterCommandMode);
    }

    #[test]
    fn ctrl_caret_asks_for_the_alternate_buffer() {
        let mut input = Input::default();
        let cmd = input
            .on_key(ctrl('^'), &Mode::Normal, ContentKind::Text)
            .expect("Ctrl-^ produced no command");
        assert_eq!(cmd.action, Action::Buffer(BufferCmd::Alternate));
    }

    /// Feeds `keys` and returns the one command they produce, asserting that
    /// every key before the last resolved to nothing.
    fn typed(keys: &str) -> Command {
        let mut input = Input::default();
        let mut last = None;
        for (i, c) in keys.chars().enumerate() {
            let out = input.on_key(key(c), &Mode::Normal, ContentKind::Text);
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
            last = input.on_key(key(c), &Mode::Normal, ContentKind::Text);
        }
        last.unwrap_or_else(|| panic!("{keys:?} produced no command"))
    }

    fn nothing(keys: &str) -> Option<Command> {
        let mut input = Input::default();
        let mut last = None;
        for c in keys.chars() {
            last = input.on_key(key(c), &Mode::Normal, ContentKind::Text);
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
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::WordForward),
                count: 1,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn cw_carries_the_change_operator() {
        assert_eq!(
            typed("cw").action,
            Action::Operate {
                op: Operator::Change,
                target: Target::Motion(Motion::WordForward),
                count: 1,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn the_doubled_form_is_the_current_line() {
        assert_eq!(
            typed("dd").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::CurrentLine),
                count: 1,
                sink: Sink::Ring
            }
        );
        assert_eq!(
            typed("cc").action,
            Action::Operate {
                op: Operator::Change,
                target: Target::Motion(Motion::CurrentLine),
                count: 1,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn counts_on_both_sides_multiply() {
        assert_eq!(
            typed("2d3w").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::WordForward),
                count: 6,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn a_count_after_the_operator_stands_alone() {
        assert_eq!(
            typed("d3w").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::WordForward),
                count: 3,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn zero_after_an_operator_is_the_line_start_motion_not_a_count() {
        assert_eq!(
            typed("d0").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::LineStart),
                count: 1,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn an_operator_reaches_through_the_g_prefix() {
        assert_eq!(
            typed("dgg").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::FirstLine),
                count: 1,
                sink: Sink::Ring
            }
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
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::Line(5)),
                count: 1,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn yank_is_an_operator_like_the_others() {
        assert_eq!(
            typed("yw").action,
            Action::Operate {
                op: Operator::Yank,
                target: Target::Motion(Motion::WordForward),
                count: 1,
                sink: Sink::Ring
            }
        );
        assert_eq!(
            typed("yy").action,
            Action::Operate {
                op: Operator::Yank,
                target: Target::Motion(Motion::CurrentLine),
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
                target: Target::Motion(Motion::CurrentLine),
                count: 1,
                sink: Sink::Ring
            }
        );
        assert_eq!(
            typed("x").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::Right),
                count: 1,
                sink: Sink::Ring
            }
        );
        assert_eq!(
            typed("5x").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::Right),
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
                target: Target::Motion(Motion::CurrentLine),
                count: 1,
                sink: Sink::BlackHole
            }
        );
        assert_eq!(
            typed("\"_dw").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::WordForward),
                count: 1,
                sink: Sink::BlackHole
            }
        );
    }

    #[test]
    fn the_black_hole_does_not_leak_into_the_next_command() {
        let mut input = Input::default();
        for c in "\"_dd".chars() {
            input.on_key(key(c), &Mode::Normal, ContentKind::Text);
        }
        assert_eq!(
            typed_with(&mut input, "dd").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::CurrentLine),
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
        assert_eq!(typed("\"p").action, Action::OpenPicker(PickerKind::Register { before: false }));
        assert_eq!(typed("\"P").action, Action::OpenPicker(PickerKind::Register { before: true }));
    }

    #[test]
    fn picker_keys_map_to_pick_actions() {
        let mut input = Input::default();
        let mut act = |k: Key| input.on_key(k, &Mode::Pick, ContentKind::Text).unwrap().action;

        assert_eq!(act(key('a')), Action::PickChar('a'));
        assert_eq!(act(ctrl('n')), Action::PickNext);
        assert_eq!(act(ctrl('p')), Action::PickPrev);
        assert_eq!(act(ctrl('a')), Action::PickToggleShort);
        assert_eq!(act(Key::code(KeyCode::Enter)), Action::PickAccept);
        assert_eq!(act(Key::code(KeyCode::Esc)), Action::PickCancel);
        assert_eq!(act(Key::code(KeyCode::Backspace)), Action::PickBackspace);
        assert_eq!(act(Key::code(KeyCode::Down)), Action::PickNext);
        assert_eq!(act(Key::code(KeyCode::Up)), Action::PickPrev);
    }

    /// `p` is a literal in the picker's query, not the paste key.
    #[test]
    fn a_plain_p_in_the_picker_is_a_query_char() {
        let mut input = Input::default();
        assert_eq!(
            input.on_key(key('p'), &Mode::Pick, ContentKind::Text).unwrap().action,
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
                target: Target::Motion(Motion::CurrentLine),
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
        input.on_key(key('2'), &Mode::Normal, ContentKind::Text);
        input.on_key(key('d'), &Mode::Normal, ContentKind::Text);
        assert_eq!(input.pending_display(), "2d");

        input.on_key(Key::code(KeyCode::Esc), &Mode::Normal, ContentKind::Text);
        assert_eq!(input.pending_display(), "");
    }

    #[test]
    fn the_pending_display_shows_the_whole_half_typed_command() {
        let mut input = Input::default();
        for c in "2d3".chars() {
            input.on_key(key(c), &Mode::Normal, ContentKind::Text);
        }
        assert_eq!(input.pending_display(), "2d3");
        input.on_key(key('g'), &Mode::Normal, ContentKind::Text);
        assert_eq!(input.pending_display(), "2d3g");
    }

    #[test]
    fn u_undoes_and_ctrl_r_redoes() {
        let mut input = Input::default();
        assert_eq!(
            input.on_key(key('u'), &Mode::Normal, ContentKind::Text).unwrap().action,
            Action::Undo
        );
        assert_eq!(
            input.on_key(ctrl('r'), &Mode::Normal, ContentKind::Text).unwrap().action,
            Action::Redo
        );
    }

    #[test]
    fn undo_and_redo_take_a_count() {
        let mut input = Input::default();
        assert_eq!(typed("3u").count, 3);

        assert!(input.on_key(key('2'), &Mode::Normal, ContentKind::Text).is_none());
        let cmd = input.on_key(ctrl('r'), &Mode::Normal, ContentKind::Text).unwrap();
        assert_eq!(cmd.count, 2);
        assert_eq!(cmd.action, Action::Redo);
    }

    /// `u`, `d` and `c` are normal-mode keys, not text.
    #[test]
    fn operator_keys_are_just_letters_in_insert_mode() {
        let mut input = Input::default();
        for c in ['u', 'd', 'c'] {
            let cmd = input.on_key(key(c), &Mode::Insert, ContentKind::Text).unwrap();
            assert_eq!(cmd.action, Action::InsertChar(c));
        }
    }

    // ---- step 1: operator shorthands, r, ~, J ------------------------------

    #[test]
    fn the_operator_shorthands_expand_to_operator_plus_motion() {
        let cases = [
            ('D', Operator::Delete, Motion::LineEnd),
            ('C', Operator::Change, Motion::LineEnd),
            ('S', Operator::Change, Motion::CurrentLine),
            ('s', Operator::Change, Motion::Right),
            ('X', Operator::Delete, Motion::Left),
        ];
        for (c, op, motion) in cases {
            assert_eq!(
                typed(&c.to_string()).action,
                Action::Operate { op, target: Target::Motion(motion), count: 1, sink: Sink::Ring },
                "{c} should be the shorthand for {op:?} over {motion:?}",
            );
        }
    }

    #[test]
    fn d_and_c_shorthands_are_exactly_their_long_forms() {
        assert_eq!(typed("D").action, typed("d$").action);
        assert_eq!(typed("C").action, typed("c$").action);
        assert_eq!(typed("S").action, typed("cc").action);
        assert_eq!(typed("X").action, typed("dh").action);
    }

    #[test]
    fn r_waits_for_its_character() {
        let mut input = Input::default();
        assert!(
            input.on_key(key('r'), &Mode::Normal, ContentKind::Text).is_none(),
            "r alone resolves to nothing"
        );
        assert_eq!(input.pending_display(), "r", "and says so in the status line");
        assert_eq!(
            input.on_key(key('x'), &Mode::Normal, ContentKind::Text).unwrap().action,
            Action::ReplaceChar { ch: 'x', count: 1 }
        );
    }

    #[test]
    fn r_takes_its_argument_literally() {
        // Each of these would mean something else in normal mode.
        for c in ['5', 'd', 'g', '"', ':', 'r'] {
            assert_eq!(
                typed(&format!("r{c}")).action,
                Action::ReplaceChar { ch: c, count: 1 },
                "r{c} should write a literal {c}",
            );
        }
    }

    #[test]
    fn a_count_before_r_belongs_to_the_replace() {
        assert_eq!(typed("3rx").action, Action::ReplaceChar { ch: 'x', count: 3 });
    }

    #[test]
    fn esc_abandons_a_pending_r() {
        let mut input = Input::default();
        input.on_key(key('r'), &Mode::Normal, ContentKind::Text);
        input.on_key(Key::code(KeyCode::Esc), &Mode::Normal, ContentKind::Text);
        assert_eq!(input.pending_display(), "");
        // The next key is an ordinary command again, not r's argument.
        assert_eq!(
            input.on_key(key('x'), &Mode::Normal, ContentKind::Text).unwrap().action,
            typed("x").action
        );
    }

    #[test]
    fn tilde_and_j_fold_their_counts_in() {
        assert_eq!(typed("~").action, Action::ToggleCase { count: 1 });
        assert_eq!(typed("3~").action, Action::ToggleCase { count: 3 });
        assert_eq!(typed("J").action, Action::JoinLines { count: 1 });
        assert_eq!(typed("4J").action, Action::JoinLines { count: 4 });
    }

    // ---- step 2: find-char and text objects --------------------------------

    fn find(ch: char, forward: bool, till: bool) -> Motion {
        Motion::FindChar { ch, forward, till, repeat: false }
    }

    #[test]
    fn the_four_find_keys_wait_for_a_character() {
        let cases = [
            ("fx", find('x', true, false)),
            ("tx", find('x', true, true)),
            ("Fx", find('x', false, false)),
            ("Tx", find('x', false, true)),
        ];
        for (keys, motion) in cases {
            assert_eq!(typed(keys).action, Action::Move(motion), "{keys}");
        }
    }

    #[test]
    fn a_find_alone_resolves_to_nothing_and_shows_in_the_status_line() {
        let mut input = Input::default();
        assert!(input.on_key(key('f'), &Mode::Normal, ContentKind::Text).is_none());
        assert_eq!(input.pending_display(), "f");
    }

    #[test]
    fn a_find_takes_its_argument_literally() {
        // Every one of these means something else in normal mode.
        for c in ['d', '5', 'i', ';', '"'] {
            assert_eq!(
                typed(&format!("f{c}")).action,
                Action::Move(find(c, true, false)),
                "f{c} should search for a literal {c}",
            );
        }
    }

    #[test]
    fn a_find_works_as_an_operator_target() {
        assert_eq!(
            typed("df,").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(find(',', true, false)),
                count: 1,
                sink: Sink::Ring,
            }
        );
    }

    #[test]
    fn counts_reach_a_find_through_an_operator() {
        let Action::Operate { count, .. } = typed("2d3f,").action else {
            panic!("expected an operator");
        };
        assert_eq!(count, 6, "counts multiply, as they do for any motion");
    }

    #[test]
    fn semicolon_and_comma_repeat_and_reverse() {
        assert_eq!(typed(";").action, Action::Move(Motion::RepeatFind { reverse: false }));
        assert_eq!(typed(",").action, Action::Move(Motion::RepeatFind { reverse: true }));
        assert_eq!(
            typed("d;").action,
            Action::Operate {
                op: Operator::Delete,
                target: Target::Motion(Motion::RepeatFind { reverse: false }),
                count: 1,
                sink: Sink::Ring,
            }
        );
    }

    #[test]
    fn esc_abandons_a_pending_find() {
        let mut input = Input::default();
        input.on_key(key('f'), &Mode::Normal, ContentKind::Text);
        input.on_key(Key::code(KeyCode::Esc), &Mode::Normal, ContentKind::Text);
        assert_eq!(input.pending_display(), "");
        assert_eq!(
            input.on_key(key('x'), &Mode::Normal, ContentKind::Text).unwrap().action,
            typed("x").action
        );
    }

    // ---- text objects ------------------------------------------------------

    fn object(object: TextObject, around: bool) -> Action {
        Action::Operate {
            op: Operator::Delete,
            target: Target::Object { object, around },
            count: 1,
            sink: Sink::Ring,
        }
    }

    #[test]
    fn diw_and_daw_reach_the_word_object() {
        assert_eq!(typed("diw").action, object(TextObject::Word { big: false }, false));
        assert_eq!(typed("daw").action, object(TextObject::Word { big: false }, true));
        assert_eq!(typed("diW").action, object(TextObject::Word { big: true }, false));
    }

    #[test]
    fn the_bracket_objects_are_named_by_their_opening_char() {
        // Either bracket of a pair selects it, and b/B are vim's aliases.
        for keys in ["di(", "di)", "dib"] {
            assert_eq!(typed(keys).action, object(TextObject::Delimited('('), false), "{keys}");
        }
        for keys in ["di{", "di}", "diB"] {
            assert_eq!(typed(keys).action, object(TextObject::Delimited('{'), false), "{keys}");
        }
        assert_eq!(typed("di[").action, object(TextObject::Delimited('['), false));
        assert_eq!(typed("di<").action, object(TextObject::Delimited('<'), false));
    }

    #[test]
    fn the_quote_objects_carry_their_quote() {
        assert_eq!(typed("di\"").action, object(TextObject::Quoted('"'), false));
        assert_eq!(typed("di'").action, object(TextObject::Quoted('\''), false));
        assert_eq!(typed("da`").action, object(TextObject::Quoted('`'), true));
    }

    #[test]
    fn ip_and_ap_reach_the_paragraph_object() {
        assert_eq!(typed("dip").action, object(TextObject::Paragraph, false));
        assert_eq!(typed("dap").action, object(TextObject::Paragraph, true));
    }

    #[test]
    fn change_and_yank_take_objects_too() {
        let expect = |op| Action::Operate {
            op,
            target: Target::Object { object: TextObject::Word { big: false }, around: false },
            count: 1,
            sink: Sink::Ring,
        };
        assert_eq!(typed("ciw").action, expect(Operator::Change));
        assert_eq!(typed("yiw").action, expect(Operator::Yank));
    }

    /// `i` and `a` only mean "text object" while an operator is waiting. On
    /// their own they still enter insert mode, or the editor would be unusable.
    #[test]
    fn i_and_a_still_enter_insert_mode_without_an_operator() {
        assert_eq!(typed("i").action, Action::EnterInsert);
        assert_eq!(typed("a").action, Action::EnterInsertAfter);
    }

    #[test]
    fn an_unknown_object_key_abandons_the_operator() {
        let mut input = Input::default();
        for c in ['d', 'i'] {
            assert!(input.on_key(key(c), &Mode::Normal, ContentKind::Text).is_none());
        }
        assert_eq!(input.pending_display(), "di");
        assert!(
            input.on_key(key('z'), &Mode::Normal, ContentKind::Text).is_none(),
            "no object named z"
        );
        assert_eq!(input.pending_display(), "", "and the operator is dropped");
    }

    #[test]
    fn esc_abandons_a_pending_object() {
        let mut input = Input::default();
        input.on_key(key('d'), &Mode::Normal, ContentKind::Text);
        input.on_key(key('i'), &Mode::Normal, ContentKind::Text);
        input.on_key(Key::code(KeyCode::Esc), &Mode::Normal, ContentKind::Text);
        assert_eq!(input.pending_display(), "");
    }
}
