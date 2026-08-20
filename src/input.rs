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

use crate::config::{Bind, KeyMode, Keymap, Lookup};
use crate::editor::{Action, BufferCmd, Command, FileOp, Mode, TreeCmd, VisualKind, WindowCmd};
use crate::key::{Key, KeyCode};
use crate::motion::{Motion, Operator, Target, TextObject};
use crate::picker::PickerKind;
use crate::registers::Sink;
use crate::tree::ClipMode;
use crate::window::{ContentKind, Dir, Side};

/// Where a surround command has got to.
///
/// `ys` is the interesting one: it wants a motion, and rather than a second
/// copy of the motion machinery it leaves the yank operator pending and is
/// intercepted where a motion resolves — which is how `ysiw"`, `ys2w)` and
/// `ysip>` all work without a line of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surround {
    /// `ys` — waiting for the motion or object that says what to wrap.
    Add,
    /// The target arrived; waiting for the character to wrap it in.
    AddWith(Target, usize),
    /// `ds` — waiting for the character to remove.
    Delete,
    /// `cs` — waiting for the character to replace, and then for its
    /// replacement.
    Change(Option<char>),
    /// Visual `S` — waiting for the character to wrap the selection in.
    Selection,
}

#[derive(Default)]
pub struct Input {
    count: Option<usize>,
    /// The count typed *after* an operator — the `3` of `d3w`.
    motion_count: Option<usize>,
    operator: Option<Operator>,
    g_pending: bool,
    /// `"` has been typed and is waiting for the register it names.
    quote_pending: bool,
    /// `ys`, `ds`, `cs` and visual `S` — see `docs/specs/surround.md`.
    surround: Option<Surround>,
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
    /// The user's rewrites, from `[keys.*]`. Empty until a frontend calls
    /// [`Input::set_keys`], and empty is the default: every key means what it
    /// always meant.
    keys: Keymap,
    /// `d` has been typed in a tree and is waiting to see whether the next key
    /// is the second `d`. Not a key on its own, the same way `Ctrl-W` is not.
    delete_pending: bool,
    /// Keys typed that begin a binding in the user's keymap without completing
    /// one yet — the `<Space>` of a half-typed `<leader>e`. Pending state, so
    /// `reset` clears it; the keymap beside it is configuration and survives.
    remap_pending: Vec<Key>,
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
        'w' => Motion::Word { big: false, forward: true, end: false },
        'b' => Motion::Word { big: false, forward: false, end: false },
        'e' => Motion::Word { big: false, forward: true, end: true },
        'W' => Motion::Word { big: true, forward: true, end: false },
        'B' => Motion::Word { big: true, forward: false, end: false },
        'E' => Motion::Word { big: true, forward: true, end: true },
        '0' => Motion::LineStart,
        // `^` was an alias for `0` until the first-non-blank motion existed.
        '^' => Motion::FirstNonBlank,
        '$' => Motion::LineEnd,
        '%' => Motion::MatchingBracket,
        '{' => Motion::Paragraph { forward: false },
        '}' => Motion::Paragraph { forward: true },
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

/// What the keymap says about a key on its way in.
enum Remapped {
    /// The key means itself, and nothing was rewritten.
    Same(Key),
    /// A binding fired: feed these keys through the grammar instead.
    Keys(Vec<Key>),
    /// A `:` line, from a binding like `"<leader>d" = ":bd<CR>"`. Without the
    /// `<CR>` it is prefilled rather than run.
    Ex { line: String, run: bool },
    /// Swallowed — either unbound, or the start of a binding that is still
    /// waiting for its next key.
    Nothing,
}

impl Input {
    /// Installs the user's keymap. Called by the frontend after the config is
    /// loaded and after every `:reload`.
    ///
    /// Any half-typed sequence goes with the old map: it was a prefix of a
    /// binding that may no longer exist, and resolving it against the new one
    /// would be answering a question nobody asked.
    pub fn set_keys(&mut self, keys: Keymap) {
        self.keys = keys;
        self.remap_pending.clear();
    }

    /// Whether a command has already started and is holding out for a specific
    /// next key.
    ///
    /// That key is the command's **argument**, not a binding, and arguments are
    /// never looked up in the keymap: `r<Space>` has to write a space even
    /// when `<Space>` is the leader, and `f<Space>` has to find one. The same
    /// goes for the keys that only exist inside a sequence — `gg`, `<C-w>s`,
    /// the tree's `dd`, and the object of `di(`.
    ///
    /// A count and a pending operator are deliberately absent: after `d` the
    /// next key is a fresh motion lookup, which is what makes a rebound `w`
    /// also rebind `dw`.
    fn mid_command(&self) -> bool {
        self.replace_pending
            || self.surround.is_some()
            || self.quote_pending
            || self.find_pending.is_some()
            || self.object_pending.is_some()
            || self.window_pending
            || self.g_pending
            || self.delete_pending
    }

    /// What `[keys.normal]` lends to another mode for the keys held so far.
    ///
    /// **Visual borrows everything.** `input.rs` falls through to `normal` for
    /// anything visual does not claim, so a motion rebound in `[keys.normal]`
    /// has to be rebound here too or `v` then `j` would disagree with a bare
    /// `j`.
    ///
    /// **A tree borrows the keys it has no meaning for.** Its own vocabulary
    /// comes first, which is what stops `"j" = "left"` from turning `j` into
    /// "collapse" in a pane sitting on a filesystem. Everything else falls
    /// through: `"<C-b>" = "window_tree"` has to close the sidebar it opened,
    /// and nothing in a tree spells `<C-b>`.
    ///
    /// The test is the *key*, not how long the binding is. Borrowing sequences
    /// only stood in for this first, and got the common case wrong — one key
    /// bound to `window_tree` opened the tree and then could not put it away,
    /// because the second press happened with the tree focused.
    ///
    /// Claiming a prefix claims the whole sequence: `g` is the tree's `gg` and
    /// `gh`, so a normal-mode `"gd"` never fires here. The check is on the
    /// first key for that reason, and it runs ahead of [`Lookup::Prefix`] so a
    /// half-typed borrow cannot swallow a key the tree needs.
    fn borrowed_from_normal(&self, mode: KeyMode) -> Lookup {
        let found = match mode {
            KeyMode::Normal => return Lookup::Miss,
            _ => self.keys.lookup(KeyMode::Normal, &self.remap_pending),
        };
        match mode {
            KeyMode::Visual => found,
            _ if self.remap_pending.first().is_some_and(|&k| self.tree_claims(k)) => Lookup::Miss,
            _ => found,
        }
    }

    /// Rewrites `key` into the keys the user's config says it means.
    ///
    /// One pass, never chained: with `j = "left"` and `h = "right"` the two
    /// swap rather than one of them winning, because each is looked up against
    /// bi's own keys and not against the other's result. A binding's target is
    /// fed through the grammar without passing here again, for the same
    /// reason.
    ///
    /// A key that begins a longer binding is held rather than resolved. If the
    /// next key does not continue it, the held keys are dropped — they had no
    /// meaning of their own to fall back on — and the one that broke the
    /// sequence is looked up afresh, so it still does what it always did.
    ///
    /// Nothing is remapped in the modes that are literal text entry — insert,
    /// replace, the command line, the search line and the picker. There is no
    /// binding there to change, and rewriting a keystroke into another
    /// character is the one thing a keymap must never do to text.
    fn remap(&mut self, key: Key, mode: &Mode, content: ContentKind) -> Remapped {
        let mode = match mode {
            Mode::Normal if content == ContentKind::Tree => KeyMode::Tree,
            Mode::Normal => KeyMode::Normal,
            Mode::Visual(_) => KeyMode::Visual,
            _ => {
                self.remap_pending.clear();
                return Remapped::Same(key);
            }
        };
        if self.keys.is_empty() || (self.remap_pending.is_empty() && self.mid_command()) {
            return Remapped::Same(key);
        }

        self.remap_pending.push(key);
        loop {
            let mut found = self.keys.lookup(mode, &self.remap_pending);
            if found == Lookup::Miss {
                found = self.borrowed_from_normal(mode);
            }
            match found {
                Lookup::Bound(Bind::Keys(to)) => {
                    self.remap_pending.clear();
                    return Remapped::Keys(to);
                }
                // An ex line is not keys and never goes through the grammar:
                // it is the command, not the typing of it.
                Lookup::Bound(Bind::Ex { line, run }) => {
                    self.remap_pending.clear();
                    return Remapped::Ex { line, run };
                }
                // Unbound. Swallowed rather than passed on, or `"h" = false`
                // would still move left.
                Lookup::Unbound => {
                    self.remap_pending.clear();
                    return Remapped::Nothing;
                }
                Lookup::Prefix => return Remapped::Nothing,
                // A dead end. Anything held was a prefix and means nothing on
                // its own, so it goes, and `key` starts again from the root —
                // where a second miss can only be a plain unmapped key.
                Lookup::Miss if self.remap_pending.len() > 1 => {
                    self.remap_pending.clear();
                    self.remap_pending.push(key);
                }
                Lookup::Miss => {
                    self.remap_pending.clear();
                    return Remapped::Same(key);
                }
            }
        }
    }

    pub fn on_key(&mut self, key: Key, mode: &Mode, content: ContentKind) -> Option<Command> {
        match self.remap(key, mode, content) {
            Remapped::Same(key) => self.dispatch(key, mode, content),
            // A target may be several keys — `gg`, `<C-w>s`, a tree's `dd`.
            // They go through the grammar one at a time exactly as if typed,
            // so the earlier ones set the pending state the last one needs.
            // Only the last can resolve to a command: every key before it is a
            // prefix, by construction of the names table.
            Remapped::Keys(keys) => {
                let mut resolved = None;
                for key in keys {
                    resolved = self.dispatch(key, mode, content).or(resolved);
                }
                resolved
            }
            // Straight to a command: an ex line has no keys to dispatch, and
            // the count does not repeat it — `3<leader>d` deletes one buffer.
            Remapped::Ex { line, run } => {
                Some(Command { count: 1, action: Action::Ex { line, run } })
            }
            Remapped::Nothing => None,
        }
    }

    fn dispatch(&mut self, key: Key, mode: &Mode, content: ContentKind) -> Option<Command> {
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
            Mode::Label => Self::label(key),
            Mode::Find => Self::find(key),
        }
    }

    /// Letters are on screen and the next key picks one.
    ///
    /// Every key means one thing here, which is why labels are a mode: a
    /// character goes to the resolver, and anything else — `Esc` included —
    /// puts the letters away. See `docs/specs/labels.md`.
    fn label(key: Key) -> Option<Command> {
        let action = match key.code {
            KeyCode::Char(c) if !key.mods.ctrl => Action::LabelChar(c),
            _ => Action::LabelCancel,
        };
        Some(Command { count: 1, action })
    }

    /// `s` is aiming: a character either narrows what is matched or picks a
    /// letter, and the editor decides which — it is the one that knows what
    /// the letters are. See `docs/specs/find.md`.
    fn find(key: Key) -> Option<Command> {
        let action = match key.code {
            KeyCode::Char(c) if !key.mods.ctrl => Action::FindChar(c),
            KeyCode::Backspace => Action::FindBackspace,
            _ => Action::FindCancel,
        };
        Some(Command { count: 1, action })
    }

    /// What's been typed but not yet resolved, for the status line.
    pub fn pending_display(&self) -> String {
        let mut s = String::new();
        // A half-typed binding comes first because it was typed first, and
        // showing it is what stops a leader from looking like a hang.
        if !self.remap_pending.is_empty() {
            s.push_str(&crate::config::spell(&self.remap_pending));
        }
        if let Some(n) = self.count {
            s.push_str(&n.to_string());
        }
        match self.surround {
            Some(Surround::Add) | Some(Surround::AddWith(..)) => s.push_str("ys"),
            Some(Surround::Delete) => s.push_str("ds"),
            Some(Surround::Change(None)) => s.push_str("cs"),
            Some(Surround::Change(Some(of))) => {
                s.push_str("cs");
                s.push(of);
            }
            Some(Surround::Selection) => s.push('S'),
            None => {}
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
            Some(Operator::Indent { right }) => s.push(if right { '>' } else { '<' }),
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
        if self.delete_pending {
            s.push('d');
        }
        s
    }

    fn reset(&mut self) {
        // Everything *except* the keymap. `reset` clears a half-typed
        // command — a count, an operator, a pending argument — and the
        // keymap is configuration that happens to live on the same struct.
        // A plain `*self = Self::default()` dropped it after the first key
        // that resolved, so a rebound `j` worked once and then stopped.
        let keys = std::mem::take(&mut self.keys);
        *self = Self { keys, ..Self::default() };
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

    /// The character a half-typed surround is waiting for, if it is waiting
    /// for one.
    ///
    /// `Some` means the key was consumed — the inner `Option` is the command
    /// it produced, or `None` for `cs`'s first character, which only says
    /// which pair to change.
    fn surround_char(&mut self, c: char) -> Option<Option<Command>> {
        let action = match self.surround? {
            // Still waiting for a motion, so this key is not ours.
            Surround::Add => return None,
            Surround::AddWith(target, count) => Action::Surround { target, count, with: c },
            Surround::Delete => Action::Unsurround { of: c },
            Surround::Change(None) => {
                self.surround = Some(Surround::Change(Some(c)));
                return Some(None);
            }
            Surround::Change(Some(of)) => Action::Resurround { of, with: c },
            Surround::Selection => Action::SurroundSelection { with: c },
        };
        self.reset();
        Some(Some(Command { count: 1, action }))
    }

    /// Resolves a motion: applies the pending operator to it, or just moves.
    fn resolve(&mut self, motion: Motion) -> Option<Command> {
        // An absolute motion already spent the count naming its destination —
        // `d5G` deletes to line 5 once, not five times.
        let count = if motion.is_absolute() { 1 } else { self.fold_count() };
        // `ys` wants the motion and then a character, so the motion stops here
        // rather than becoming a command.
        if self.surround == Some(Surround::Add) {
            self.operator = None;
            self.surround = Some(Surround::AddWith(Target::Motion(motion), count));
            return None;
        }
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
        let count = self.fold_count();
        if self.surround == Some(Surround::Add) {
            self.operator = None;
            self.surround = Some(Surround::AddWith(Target::Object { object, around }, count));
            return None;
        }
        let op = self.operator?;
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
            // `e` for the pane most editors call an explorer. Under the window
            // prefix because it makes a window, which is where every other key
            // that makes one lives.
            KeyCode::Char('e') => WindowCmd::Tree,
            // `f` for focus: a letter on every window, and the next key goes
            // there. Not `<Tab>`, which is `Ctrl-I` byte for byte and would
            // take buffer-next with it — see `docs/specs/labels.md`.
            KeyCode::Char('f') => WindowCmd::Pick,
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
    /// Whether the tree's own keymap has a meaning for `key`.
    ///
    /// [`Input::tree`]'s allowlist as a predicate, prefixes included — the
    /// question [`Input::borrowed_from_normal`] asks before letting a
    /// `[keys.normal]` binding through to a tree. The two are one list written
    /// twice, and `tree_claims_is_the_tree_dispatchers_own_allowlist` runs
    /// every key through both rather than trusting them to stay that way.
    ///
    /// A tighter answer would be to *ask* the dispatcher, but it answers by
    /// mutating: a prefix leaves state behind and an unclaimed key resets. The
    /// predicate is the price of asking without committing.
    fn tree_claims(&self, key: Key) -> bool {
        let ctrl = key.mods.ctrl;
        match key.code {
            // A count, on the same terms the dispatcher takes one: a leading
            // `0` is not a count, it is a key the tree does not have.
            KeyCode::Char(c) if c.is_ascii_digit() => !(c == '0' && self.count.is_none()),
            // The window prefix, the two half-page keys, the three that ask to
            // see a buffer, and the file picker.
            KeyCode::Char('w' | 'u' | 'i' | 'o' | 'p' | '^') if ctrl => true,
            // The second half of `gf`, and only while the `g` is armed: a bare
            // `f` is not a tree key, so a `[keys.normal]` binding may have it.
            KeyCode::Char('f') if self.g_pending => true,
            // Movement, marks, the two prompts, the command line, and `g` and
            // `d`, which are prefixes rather than keys.
            KeyCode::Char(
                'h' | 'j' | 'k' | 'l' | 'g' | 'd' | 'G' | 'R' | 'y' | 'c' | 'x' | 'p' | 'a' | 'r'
                | '-' | '+' | ':',
            ) => true,
            KeyCode::Enter | KeyCode::Esc | KeyCode::Tab => true,
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => true,
            _ => false,
        }
    }

    /// Nothing here enters insert or visual mode, and `Ctrl-W` is normal-mode
    /// only, so a tree can never be focused in either.
    fn tree(&mut self, key: Key) -> Option<Command> {
        if self.window_pending {
            return self.window_key(key);
        }
        // `dd` is the whole key. Anything else drops it rather than being
        // swallowed, which is the rule `Ctrl-W` already follows — and here it
        // is what stops a mistyped `d` from deleting on the next keystroke.
        if std::mem::take(&mut self.delete_pending) {
            return match key.code {
                KeyCode::Char('d') => self.plain(Action::Tree(TreeCmd::Delete)),
                _ => {
                    self.reset();
                    None
                }
            };
        }
        let ctrl = key.mods.ctrl;
        let count = self.count.unwrap_or(1).max(1);
        let g = std::mem::take(&mut self.g_pending);

        let tree_cmd = match key.code {
            // The `g` prefix resolves first, or `gh` would read as `h`.
            KeyCode::Char('g') if g => TreeCmd::First,
            KeyCode::Char('h') if g => TreeCmd::ToggleHidden,
            // `gf` is "go to a thing by name" in both maps. In a text window
            // the things are buffers; here they are the rows on this pane, and
            // taking one moves the cursor to it rather than opening it.
            KeyCode::Char('f') if g => {
                return self.plain(Action::OpenPicker(PickerKind::TreeRow));
            }
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
            // Asking to see a buffer here, which is what `:bn` means in a tree.
            KeyCode::Char('i') if ctrl => return self.plain(Action::Buffer(BufferCmd::Next)),
            KeyCode::Char('o') if ctrl => return self.plain(Action::Buffer(BufferCmd::Prev)),
            KeyCode::Tab => return self.plain(Action::Buffer(BufferCmd::Next)),
            // Before `p`, which is paste: a tree is a place you look files up,
            // so the key that looks one up by name belongs here more than
            // anywhere. Without this it read as `p` and pasted.
            KeyCode::Char('p') if ctrl => {
                return self.plain(Action::OpenPicker(PickerKind::File));
            }

            KeyCode::Char('j') | KeyCode::Down => TreeCmd::Select { down: true, count },
            KeyCode::Char('k') | KeyCode::Up => TreeCmd::Select { down: false, count },
            KeyCode::Char('d') if ctrl => TreeCmd::HalfPage { down: true },
            KeyCode::Char('u') if ctrl => TreeCmd::HalfPage { down: false },
            KeyCode::Char('G') => TreeCmd::Last,
            KeyCode::Char('l') | KeyCode::Right => TreeCmd::Expand,
            KeyCode::Char('h') | KeyCode::Left => TreeCmd::Collapse,
            KeyCode::Enter => TreeCmd::Enter,
            KeyCode::Char('-') => TreeCmd::Up,
            KeyCode::Char('+') => TreeCmd::Down,
            KeyCode::Char('R') => TreeCmd::Refresh,

            KeyCode::Char('y') => TreeCmd::Yank,
            KeyCode::Char('c') => TreeCmd::Mark(ClipMode::Copy),
            KeyCode::Char('x') => TreeCmd::Mark(ClipMode::Cut),
            KeyCode::Char('p') => TreeCmd::Paste,
            // The only way out that is not the right key on every one of them.
            KeyCode::Esc => TreeCmd::ClearMarks,

            // These three only fill the command line in; the work is done by
            // `:create`, `:rename` and `:delete`, which are ordinary ex
            // commands. `d` is free here because `Ctrl-D` was matched above.
            KeyCode::Char('a') => TreeCmd::Prompt(FileOp::Create),
            KeyCode::Char('r') => TreeCmd::Prompt(FileOp::Rename),
            // `d` is the start of `dd`, not a key. Deleting is the one thing
            // here that no `:` line stands in front of.
            KeyCode::Char('d') => {
                self.delete_pending = true;
                return None;
            }

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

        // A surround waiting for its character takes the next one whatever it
        // would otherwise have meant — `ds(` must not read the `(` as a motion.
        if let KeyCode::Char(c) = key.code
            && !ctrl
            && let Some(command) = self.surround_char(c)
        {
            return command;
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
            // The picker over every file under the session's root — see
            // `docs/specs/files.md`.
            KeyCode::Char('p') if ctrl => self.plain(Action::OpenPicker(PickerKind::File)),
            KeyCode::Char('n') if ctrl => self.plain(Action::AddCursorNextMatch),
            KeyCode::Char('x') if ctrl => self.plain(Action::SkipCursorToNextMatch),
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
            // The first thing in the keymap to read `shift`, which `Key` has
            // carried since it was written. Terminals that do not send a
            // modifier with an arrow simply get the plain arrow, which still
            // moves the cursor — and `:m` works everywhere.
            KeyCode::Down if key.mods.shift => self.plain(Action::MoveLines { down: true }),
            KeyCode::Up if key.mods.shift => self.plain(Action::MoveLines { down: false }),
            // Vim spells its jump list this way; bi has no jump list and
            // these are the keys the fingers reach for. Checked before the
            // plain `i` and `o`, which would otherwise swallow them — and
            // `Tab` is listed because it *is* Ctrl-I, byte for byte.
            KeyCode::Char('i') if ctrl => self.plain(Action::Buffer(BufferCmd::Next)),
            KeyCode::Char('o') if ctrl => self.plain(Action::Buffer(BufferCmd::Prev)),
            // `Ctrl-Tab` for the buffer switcher, where the terminal sends
            // one — kitty and alacritty do, with the protocol that tells
            // `Ctrl-I` and `Ctrl-Tab` apart. Where it does not, this arm never
            // fires and the plain `Tab` below is what arrives, which is
            // exactly what it always did. See `docs/specs/buffers.md`.
            KeyCode::Tab if ctrl => self.plain(Action::Buffer(BufferCmd::List)),
            KeyCode::Tab => self.plain(Action::Buffer(BufferCmd::Next)),
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
            // `+` and `*` are one register here. X11's split between the
            // clipboard and the primary selection is real, but OSC 52 addresses
            // them with one code and it is not worth a second letter.
            if c == '+' || c == '*' {
                self.sink = Sink::System;
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
                'e' => self.resolve(Motion::Word { big: false, forward: false, end: true }),
                'E' => self.resolve(Motion::Word { big: true, forward: false, end: true }),
                '_' => self.resolve(Motion::LastNonBlank),
                // The other file — the test beside the implementation. `ga`
                // in vim prints a character code, which nothing here does,
                // and a leader binding can spell it `<leader>a` instead.
                // See `docs/specs/alternate.md`.
                'a' => self.plain(Action::Ex { line: "alt".into(), run: true }),
                // The buffer switcher, for the terminals that cannot tell
                // `Ctrl-Tab` from `Tab`. Vim's `gf` opens the file named under
                // the cursor; bi has no such command, and the letter is the
                // one people reach for when they mean "go to a file".
                // See `docs/specs/buffers.md`.
                'f' => self.plain(Action::Buffer(BufferCmd::List)),
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
                (Operator::Delete, 'd')
                    | (Operator::Change, 'c')
                    | (Operator::Yank, 'y')
                    | (Operator::Indent { right: true }, '>')
                    | (Operator::Indent { right: false }, '<')
            );
            if doubled {
                return self.resolve(Motion::CurrentLine);
            }
            // `ys`, `ds`, `cs` — `s` is not a motion, so all three are
            // sequences vim leaves unused and nothing has to be given up to
            // have them. See `docs/specs/surround.md`.
            if c == 's' && self.surround.is_none() {
                self.surround = match op {
                    Operator::Yank => Some(Surround::Add),
                    Operator::Delete => Some(Surround::Delete),
                    Operator::Change => Some(Surround::Change(None)),
                    Operator::Indent { .. } => None,
                };
                if self.surround.is_some() {
                    // `ys` keeps the yank operator pending, because what
                    // follows is a motion and the machinery for that is the
                    // operator's.
                    if self.surround != Some(Surround::Add) {
                        self.operator = None;
                    }
                    return None;
                }
            }
            // `yss` — the whole line, the doubled form of `ys`.
            if c == 's' && self.surround == Some(Surround::Add) {
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
                let sink = self.sink;
                self.reset();
                return Some(Command {
                    count: 1,
                    action: Action::Paste { before: c == 'P', count, sink },
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
            // Vim's `S` is `cc` spelled shorter, and `cc` still spells it.
            // In visual mode `S` stays vim-surround's, which is a different
            // key in a different mode. See `docs/specs/scopes.md`.
            'S' => return self.plain(Action::ShowScopes),
            // Vim's `s` is `cl` spelled shorter, and `cl` still works. This
            // is the better use of the key — see `docs/specs/find.md`.
            's' => return self.plain(Action::EnterFind),
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
            '>' | '<' => {
                self.operator = Some(Operator::Indent { right: c == '>' });
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
        // `Ctrl-X` sits beside it: the same search, but the match is passed
        // over rather than taken.
        //
        // Both are intercepted here even in a block, where neither applies,
        // because `normal` binds them too and anything falling through to it
        // would arrive there anyway — the exclusion has to refuse the key, not
        // merely decline to handle it.
        if ctrl && matches!(key.code, KeyCode::Char('n') | KeyCode::Char('x')) {
            if block {
                return None;
            }
            return self.plain(match key.code {
                KeyCode::Char('n') => Action::AddCursorNextMatch,
                _ => Action::SkipCursorToNextMatch,
            });
        }

        let KeyCode::Char(c) = key.code else {
            return self.normal(key);
        };
        if ctrl {
            return self.normal(key);
        }

        // A surround waiting for its character takes the next one, before the
        // objects and operators below can read it as something else.
        if let Some(command) = self.surround_char(c) {
            return command;
        }
        // vim-surround's visual key. Normal mode's `S` is `cc` and stays that
        // way; this one only exists where there is a selection to wrap.
        if c == 'S' {
            self.surround = Some(Surround::Selection);
            return None;
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
            '>' | '<' => Some(Operator::Indent { right: c == '>' }),
            _ => None,
        };
        if let Some(op) = op {
            let sink = self.sink;
            // The count is steps here, not rows — the selection already says
            // which rows — so it stays on the command rather than being folded
            // into a range. `3>` is the command three times.
            let count = match op {
                Operator::Indent { .. } => self.fold_count(),
                _ => 1,
            };
            self.reset();
            return Some(Command { count, action: Action::OperateSelection { op, sink } });
        }

        // `p` here replaces the selection rather than inserting beside it, so
        // it cannot fall through to normal mode's put. `P` is the same paste
        // that keeps the ring — "before the cursor" means nothing when the
        // selection says exactly where the text goes.
        //
        // Not while `"` is waiting: there the `p` names the picker, and the
        // register it lands on is pasted over the selection all the same.
        if (c == 'p' || c == 'P') && !self.quote_pending {
            if self.sink == Sink::BlackHole {
                // Nothing ever reaches the black hole, so nothing comes out.
                self.reset();
                return None;
            }
            let count = self.fold_count();
            let sink = self.sink;
            self.reset();
            return Some(Command {
                count: 1,
                action: Action::PasteSelection { capture: c == 'p', count, sink },
            });
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
            // Not an `InsertChar('\t')`: where the next stop is depends on
            // where on the line the cursor already is, and with `expandtab`
            // there is no tab to insert. Shift-Tab is the way back.
            KeyCode::Tab => Action::InsertIndent { right: !key.mods.shift },
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
            // The shells' key for exactly this, and vim's register-insert key
            // on a command line bi does not have. Above the char arm, which
            // would otherwise take it and type a literal `r`.
            KeyCode::Char('r') if ctrl => Action::OpenPicker(PickerKind::History),
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

    fn shifted(code: KeyCode) -> Key {
        Key { code, mods: crate::key::Mods { shift: true, ..Default::default() } }
    }

    /// Feeds `keys` in normal mode and returns what the last one resolved to.
    fn normal_keys(keys: &str) -> Option<Command> {
        let mut input = Input::default();
        let mut last = None;
        for c in keys.chars() {
            last = input.on_key(key(c), &Mode::Normal, ContentKind::Text);
        }
        last
    }

    fn motion_of(keys: &str) -> Motion {
        match normal_keys(keys).unwrap_or_else(|| panic!("{keys:?} resolved to nothing")).action {
            Action::Move(motion) => motion,
            other => panic!("{keys:?} is not a motion: {other:?}"),
        }
    }

    /// The config from the report that prompted all of this: hjkl shifted one
    /// key right, `h` unbound, and the tree following the same shape.
    fn shifted_layout() -> Input {
        let src = "\
[options]
number = 5

[keys.normal]
\"h\" = false
\"j\" = \"left\"
\"k\" = \"down\"
\"l\" = \"up\"
\";\" = \"right\"

[keys.tree]
\"k\" = \"tree_select_down\"
\"l\" = \"tree_select_up\"
\";\" = \"tree_expand\"
\"j\" = \"tree_collapse\"
\"h\" = false
";
        let (config, problems) =
            crate::config::parse(src, crate::config::Config::default()).expect("parses");
        assert!(problems.is_empty(), "{problems:?}");

        let mut input = Input::default();
        input.set_keys(config.keys);
        input
    }

    #[test]
    fn a_configured_layout_moves_the_motions_onto_new_keys() {
        let mut input = shifted_layout();
        let mut motion = |c: char| match input.on_key(key(c), &Mode::Normal, ContentKind::Text) {
            Some(cmd) => match cmd.action {
                Action::Move(m) => Some(m),
                other => panic!("{c:?} is not a motion: {other:?}"),
            },
            None => None,
        };

        assert_eq!(motion('j'), Some(Motion::Left));
        assert_eq!(motion('k'), Some(Motion::Down));
        assert_eq!(motion('l'), Some(Motion::Up));
        assert_eq!(motion(';'), Some(Motion::Right));
        assert_eq!(motion('h'), None, "unbound, and swallowed rather than still moving left");
    }

    /// The whole argument for rewriting the key rather than the command: the
    /// grammar never learns that anything changed, so operators, counts and
    /// visual mode follow for nothing.
    #[test]
    fn a_rebound_motion_still_composes_with_operators_and_counts() {
        let mut input = shifted_layout();
        let mut last = None;
        for c in "d2k".chars() {
            last = input.on_key(key(c), &Mode::Normal, ContentKind::Text);
        }
        let cmd = last.expect("d2k resolved");
        assert!(
            matches!(
                cmd.action,
                Action::Operate {
                    op: Operator::Delete,
                    target: Target::Motion(Motion::Down),
                    count: 2,
                    ..
                }
            ),
            "{:?}",
            cmd.action
        );

        // And in visual, which has no `[keys.visual]` of its own — it borrows
        // normal's, the same way `input.rs` falls through to it.
        let visual = input.on_key(key('k'), &Mode::Visual(VisualKind::Char), ContentKind::Text);
        assert_eq!(visual.expect("resolved").action, Action::Move(Motion::Down));
    }

    #[test]
    fn the_tree_has_its_own_map() {
        let mut input = shifted_layout();
        let mut act =
            |c: char| input.on_key(key(c), &Mode::Normal, ContentKind::Tree).map(|cmd| cmd.action);

        assert_eq!(act('k'), Some(Action::Tree(TreeCmd::Select { down: true, count: 1 })));
        assert_eq!(act('l'), Some(Action::Tree(TreeCmd::Select { down: false, count: 1 })));
        assert_eq!(act(';'), Some(Action::Tree(TreeCmd::Expand)));
        assert_eq!(act('j'), Some(Action::Tree(TreeCmd::Collapse)));
        assert_eq!(act('h'), None, "unbound here too");
    }

    /// Two keys that trade places must both move, not resolve through each
    /// other and collapse into one.
    #[test]
    fn a_swap_is_a_swap_and_not_a_chain() {
        let src = "[keys.normal]\n\"h\" = \"down\"\n\"j\" = \"left\"\n";
        let (config, problems) =
            crate::config::parse(src, crate::config::Config::default()).expect("parses");
        assert!(problems.is_empty(), "{problems:?}");
        let mut input = Input::default();
        input.set_keys(config.keys);

        let h = input.on_key(key('h'), &Mode::Normal, ContentKind::Text).unwrap();
        assert_eq!(h.action, Action::Move(Motion::Down));
        let j = input.on_key(key('j'), &Mode::Normal, ContentKind::Text).unwrap();
        assert_eq!(j.action, Action::Move(Motion::Left), "not Down via h");
    }

    /// A config with a leader and three bindings that spell it, which is the
    /// shape every leader test below needs.
    fn with_leader(src: &str) -> Input {
        let (config, problems) =
            crate::config::parse(src, crate::config::Config::default()).expect("parses");
        assert!(problems.is_empty(), "{problems:?}");
        let mut input = Input::default();
        input.set_keys(config.keys);
        input
    }

    fn leader_layout() -> Input {
        with_leader(
            "\
[keys]
leader = \" \"

[keys.normal]
\"<leader>e\" = \"window_tree\"
\"<leader>t\" = \"goto_first_line\"

[keys.tree]
\"<leader>d\" = \"tree_delete\"
",
        )
    }

    fn feed(input: &mut Input, keys: &str, content: ContentKind) -> Option<Command> {
        let mut last = None;
        for c in keys.chars() {
            last = input.on_key(key(c), &Mode::Normal, content);
        }
        last
    }

    /// The leader itself resolves nothing: it is the start of a binding and
    /// the second key is what fires.
    #[test]
    fn a_leader_binding_fires_on_its_last_key() {
        let mut input = leader_layout();

        assert!(input.on_key(key(' '), &Mode::Normal, ContentKind::Text).is_none(), "waiting");
        let cmd = input.on_key(key('e'), &Mode::Normal, ContentKind::Text).expect("resolved");
        assert_eq!(cmd.action, Action::Window(WindowCmd::Tree));
    }

    /// The target is two keys, and they go through the grammar exactly as if
    /// they had been typed — `gg` reaching `FirstLine` through `g_pending`.
    #[test]
    fn a_multi_key_target_travels_through_the_grammar() {
        let mut input = leader_layout();
        let cmd = feed(&mut input, " t", ContentKind::Text).expect("resolved");
        assert_eq!(cmd.action, Action::Move(Motion::FirstLine));

        // And it composes with an operator, because the grammar never learns
        // that anything was rewritten.
        let cmd = feed(&mut input, "d t", ContentKind::Text).expect("resolved");
        assert!(
            matches!(
                cmd.action,
                Action::Operate {
                    op: Operator::Delete,
                    target: Target::Motion(Motion::FirstLine),
                    ..
                }
            ),
            "{:?}",
            cmd.action
        );
    }

    /// A sequence that goes nowhere drops the prefix — which had no meaning of
    /// its own — and lets the key that broke it act normally.
    #[test]
    fn a_dead_end_drops_the_prefix_and_keeps_the_last_key() {
        let mut input = leader_layout();
        let cmd = feed(&mut input, " j", ContentKind::Text).expect("resolved");
        assert_eq!(cmd.action, Action::Move(Motion::Down), "the j still moves");

        // And the next key starts from the root again rather than continuing
        // the abandoned sequence.
        let cmd = feed(&mut input, " e", ContentKind::Text).expect("resolved");
        assert_eq!(cmd.action, Action::Window(WindowCmd::Tree));
    }

    /// `<Space>` is `Motion::Right` in bi, and binding `<leader>e` takes that
    /// away. Deliberate: there is no timeout to tell the two apart.
    #[test]
    fn a_prefix_loses_its_own_meaning() {
        let mut input = leader_layout();
        assert!(input.on_key(key(' '), &Mode::Normal, ContentKind::Text).is_none());
        assert_eq!(input.pending_display(), "<Space>", "and the status line says so");

        // Without a binding that spells it, the leader is just a key.
        let mut plain = with_leader("[keys]\nleader = \" \"\n");
        let cmd = plain.on_key(key(' '), &Mode::Normal, ContentKind::Text).expect("resolved");
        assert_eq!(cmd.action, Action::Move(Motion::Right));
    }

    /// The rule that keeps a leader from eating arguments: once a command is
    /// waiting for a specific key, that key is not a binding. Without it,
    /// `r<Space>` would type a leader instead of a space.
    #[test]
    fn an_argument_is_never_looked_up_in_the_keymap() {
        let mut input = leader_layout();
        let cmd = feed(&mut input, "r ", ContentKind::Text).expect("resolved");
        assert_eq!(cmd.action, Action::ReplaceChar { ch: ' ', count: 1 });

        let cmd = feed(&mut input, "f ", ContentKind::Text).expect("resolved");
        assert_eq!(
            cmd.action,
            Action::Move(Motion::FindChar { ch: ' ', forward: true, till: false, repeat: false })
        );
    }

    /// The tree has its own map, so a leader binding there is its own binding
    /// — and `dd` is reachable as a target now that targets can be sequences.
    #[test]
    fn a_leader_binding_reaches_the_trees_two_key_delete() {
        let mut input = leader_layout();
        let cmd = feed(&mut input, " d", ContentKind::Tree).expect("resolved");
        assert_eq!(cmd.action, Action::Tree(TreeCmd::Delete));

        // A sequence the tree does not claim is borrowed from `[keys.normal]`,
        // which is what lets one leader binding work on both sides. Its keys
        // then mean whatever they mean *here*: `gg` is the tree's first row.
        let cmd = feed(&mut input, " t", ContentKind::Tree).expect("borrowed from normal");
        assert_eq!(cmd.action, Action::Tree(TreeCmd::First));

        // A single key is not borrowed, which is the whole reason the tree has
        // a map of its own: `j` selects down here whatever normal says.
        let cmd = feed(&mut input, "j", ContentKind::Tree).expect("resolved");
        assert_eq!(cmd.action, Action::Tree(TreeCmd::Select { down: true, count: 1 }));
    }

    /// The rule the sequence-only version of this got wrong: what a tree
    /// refuses to borrow is a key it has a meaning for, not a key that happens
    /// to be typed on its own. `"<C-b>" = "window_tree"` opened the sidebar and
    /// then could not close it, because `<C-b>` is one key.
    #[test]
    fn a_normal_binding_the_tree_does_not_claim_is_borrowed() {
        let mut input =
            with_leader("[keys.normal]\n\"<C-b>\" = \"window_tree\"\n\"j\" = \"left\"\n");

        let cmd = input.on_key(ctrl('b'), &Mode::Normal, ContentKind::Text).expect("resolved");
        assert_eq!(cmd.action, Action::Window(WindowCmd::Tree));
        let cmd = input.on_key(ctrl('b'), &Mode::Normal, ContentKind::Tree).expect("borrowed");
        assert_eq!(cmd.action, Action::Window(WindowCmd::Tree), "and the same key puts it away");

        // What the tree does claim it keeps: `j` selects down whatever normal
        // says, which is the reason the tree has a map of its own at all.
        let cmd = input.on_key(key('j'), &Mode::Normal, ContentKind::Tree).expect("resolved");
        assert_eq!(cmd.action, Action::Tree(TreeCmd::Select { down: true, count: 1 }));
    }

    /// A borrowed binding whose keys mean nothing in a tree is a no-op, not an
    /// escape hatch into normal mode: the tree dispatcher is still an allowlist
    /// and `w` is not on it.
    #[test]
    fn a_borrowed_binding_the_tree_has_no_use_for_does_nothing() {
        let mut input = with_leader("[keys.normal]\n\"<C-n>\" = \"word_forward\"\n");
        assert!(input.on_key(ctrl('n'), &Mode::Normal, ContentKind::Tree).is_none());
        assert_eq!(input.pending_display(), "", "and nothing is left half-typed");
    }

    /// A tree's own map still wins over what normal lends it, whether the key
    /// is one the tree binds or the start of a sequence it binds.
    #[test]
    fn the_trees_own_keys_win_over_a_borrowed_binding() {
        // Not `with_leader`: `"gd"` takes over `g` in normal mode and says so,
        // which is the diagnostic working, not a problem with this config.
        let src = "[keys.normal]\n\"y\" = \"left\"\n\"gd\" = \"delete\"\n\"<C-d>\" = \"undo\"\n";
        let (config, _) =
            crate::config::parse(src, crate::config::Config::default()).expect("parses");
        let mut input = Input::default();
        input.set_keys(config.keys);
        assert_eq!(
            feed(&mut input, "y", ContentKind::Tree).expect("resolved").action,
            Action::Tree(TreeCmd::Yank),
            "the tree's y, not normal's h"
        );
        assert_eq!(
            feed(&mut input, "gg", ContentKind::Tree).expect("resolved").action,
            Action::Tree(TreeCmd::First),
            "g is the tree's prefix, so `gd` never gets to claim it"
        );
        assert_eq!(
            input.on_key(ctrl('d'), &Mode::Normal, ContentKind::Tree).expect("resolved").action,
            Action::Tree(TreeCmd::HalfPage { down: true })
        );
    }

    /// The predicate and the dispatcher below it are one list written twice,
    /// and this is what keeps them the same list.
    #[test]
    fn tree_claims_is_the_tree_dispatchers_own_allowlist() {
        let codes = (' '..='~').map(KeyCode::Char).chain([
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Backspace,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
        ]);
        for code in codes {
            for held in [false, true] {
                let key = Key::new(code, crate::key::Mods { ctrl: held, ..Default::default() });
                let mut input = Input::default();
                let claimed = input.tree_claims(key);
                // A key the tree has a meaning for either resolves to a
                // command or leaves something pending — a count, or the `g`,
                // `d` and `Ctrl-W` prefixes. Anything else resets and is gone.
                let acted =
                    input.tree(key).is_some() || input.mid_command() || input.count.is_some();
                assert_eq!(claimed, acted, "{}", crate::config::spell(&[key]));

                // And again with the `g` armed, which claims one more key and
                // is the state a fresh `Input` cannot show.
                let mut input = Input::default();
                input.tree(Key::char('g'));
                let claimed = input.tree_claims(key);
                let acted =
                    input.tree(key).is_some() || input.mid_command() || input.count.is_some();
                assert_eq!(claimed, acted, "after g: {}", crate::config::spell(&[key]));
            }
        }
    }

    /// Insert mode is literal text. A keymap that reached it would type the
    /// wrong characters into the buffer, which is the one unrecoverable thing
    /// a remap could do.
    #[test]
    fn a_remap_never_touches_the_modes_that_are_text() {
        let mut input = shifted_layout();
        let typed = input.on_key(key('j'), &Mode::Insert, ContentKind::Text).expect("resolved");
        assert_eq!(typed.action, Action::InsertChar('j'));

        let unbound = input.on_key(key('h'), &Mode::Insert, ContentKind::Text).expect("resolved");
        assert_eq!(unbound.action, Action::InsertChar('h'), "`h = false` does not stop typing h");
    }

    #[test]
    fn the_word_keys_name_all_eight_combinations() {
        assert_eq!(motion_of("w"), Motion::Word { big: false, forward: true, end: false });
        assert_eq!(motion_of("b"), Motion::Word { big: false, forward: false, end: false });
        assert_eq!(motion_of("e"), Motion::Word { big: false, forward: true, end: true });
        assert_eq!(motion_of("ge"), Motion::Word { big: false, forward: false, end: true });
        assert_eq!(motion_of("W"), Motion::Word { big: true, forward: true, end: false });
        assert_eq!(motion_of("B"), Motion::Word { big: true, forward: false, end: false });
        assert_eq!(motion_of("E"), Motion::Word { big: true, forward: true, end: true });
        assert_eq!(motion_of("gE"), Motion::Word { big: true, forward: false, end: true });
    }

    /// `^` was an alias for `0` until now, which the README called out as
    /// wrong. Fixing it must not disturb `0`.
    #[test]
    fn caret_and_zero_are_finally_different_keys() {
        assert_eq!(motion_of("^"), Motion::FirstNonBlank);
        assert_eq!(motion_of("0"), Motion::LineStart);
        assert_eq!(motion_of("g_"), Motion::LastNonBlank);
    }

    #[test]
    fn percent_and_the_braces_are_motions_like_any_other() {
        assert_eq!(motion_of("%"), Motion::MatchingBracket);
        assert_eq!(motion_of("}"), Motion::Paragraph { forward: true });
        assert_eq!(motion_of("{"), Motion::Paragraph { forward: false });

        // And so they compose with operators and counts for free — which is
        // the whole argument for motions being data.
        // The count rides on the operator, which is where `d2w` puts it too.
        let cmd = normal_keys("d2}").expect("resolved");
        assert!(
            matches!(
                cmd.action,
                Action::Operate {
                    op: Operator::Delete,
                    target: Target::Motion(Motion::Paragraph { forward: true }),
                    count: 2,
                    ..
                }
            ),
            "{:?}",
            cmd.action
        );
    }

    /// `0` is a count digit once a count is being typed, and `d2$` must not
    /// become `d20`. The new `^` sits next to that rule without touching it.
    #[test]
    fn zero_after_a_count_is_still_a_digit() {
        let cmd = normal_keys("20j").expect("resolved");
        assert_eq!(cmd.count, 20);
    }

    /// The first thing in the keymap to read `shift`, which `Key` has carried
    /// unread since it was written.
    #[test]
    fn shift_and_an_arrow_moves_the_line_where_the_arrow_alone_moves_the_cursor() {
        let mut input = Input::default();
        let down = input.on_key(shifted(KeyCode::Down), &Mode::Normal, ContentKind::Text);
        assert_eq!(down.unwrap().action, Action::MoveLines { down: true });

        let up = input.on_key(shifted(KeyCode::Up), &Mode::Normal, ContentKind::Text);
        assert_eq!(up.unwrap().action, Action::MoveLines { down: false });

        let plain = input.on_key(Key::code(KeyCode::Down), &Mode::Normal, ContentKind::Text);
        assert_eq!(plain.unwrap().action, Action::Move(Motion::Down), "unshifted is a motion");
    }

    #[test]
    fn a_count_says_how_far_the_line_travels() {
        let mut input = Input::default();
        assert!(input.on_key(key('3'), &Mode::Normal, ContentKind::Text).is_none());
        let cmd = input.on_key(shifted(KeyCode::Down), &Mode::Normal, ContentKind::Text).unwrap();

        assert_eq!(cmd.count, 3, "and the action is repeatable, so that is three rows");
        assert_eq!(cmd.action, Action::MoveLines { down: true });
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
        for c in ['i', 'A', 'I', 'o', 'O', 'v', 'V', 'u', 's'] {
            assert!(in_tree(&c.to_string()).is_none(), "{c:?} did something in a tree");
        }
    }

    /// `a` and `r` edit nothing themselves — each only puts a `:` line up for
    /// you to agree to.
    #[test]
    fn the_file_op_keys_ask_before_anything_happens() {
        assert_eq!(tree_action("a"), Action::Tree(TreeCmd::Prompt(FileOp::Create)));
        assert_eq!(tree_action("r"), Action::Tree(TreeCmd::Prompt(FileOp::Rename)));
    }

    /// `dd` is the exception, and it is a whole key: one `d` does nothing, and
    /// anything that is not the second `d` drops it rather than being
    /// swallowed — the rule `Ctrl-W` already follows.
    #[test]
    fn dd_is_one_key_and_a_lone_d_is_not() {
        assert!(in_tree("d").is_none(), "armed, not fired");
        assert_eq!(tree_action("dd"), Action::Tree(TreeCmd::Delete));

        assert!(in_tree("dj").is_none(), "a mistyped d does not delete on the next key");
        assert!(in_tree("dx").is_none());
    }

    #[test]
    fn minus_and_plus_re_root_in_opposite_directions() {
        assert_eq!(tree_action("-"), Action::Tree(TreeCmd::Up));
        assert_eq!(tree_action("+"), Action::Tree(TreeCmd::Down));
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

    /// `<C-n>` takes a match, `<C-x>` passes it over. They are the same
    /// gesture and have to be reachable from the same two modes, or skipping
    /// is only available where you did not need it.
    #[test]
    fn ctrl_x_skips_a_match_wherever_ctrl_n_takes_one() {
        let mut input = Input::default();
        for mode in [Mode::Normal, Mode::Visual(VisualKind::Char)] {
            let take = input.on_key(ctrl('n'), &mode, ContentKind::Text).unwrap();
            assert_eq!(take.action, Action::AddCursorNextMatch, "{mode:?}");

            let skip = input.on_key(ctrl('x'), &mode, ContentKind::Text).unwrap();
            assert_eq!(skip.action, Action::SkipCursorToNextMatch, "{mode:?}");
        }

        // Neither belongs in a block: the rectangle comes from one selection's
        // corners, so a second cursor has nothing to say about it.
        let block = Mode::Visual(VisualKind::Block);
        assert!(
            input.on_key(ctrl('x'), &block, ContentKind::Text).is_none(),
            "blockwise visual has no second selection to skip"
        );
        assert!(
            input.on_key(ctrl('n'), &block, ContentKind::Text).is_none(),
            "and none to add — the guard has to refuse the key, not fall through to `normal`"
        );
    }

    /// Vim spells the jump list this way, not the buffer list — but bi has no
    /// jump list, and these are the keys the fingers reach for. Ctrl-I *is*
    /// Tab: both are 0x09, and no terminal bi talks to tells them apart, so
    /// binding one binds the other whether or not you meant to.
    #[test]
    fn ctrl_i_and_ctrl_o_cycle_the_buffer_list() {
        let mut input = Input::default();
        let next = input.on_key(ctrl('i'), &Mode::Normal, ContentKind::Text).unwrap();
        assert_eq!(next.action, Action::Buffer(BufferCmd::Next));

        let tab = Key::code(KeyCode::Tab);
        let same = input.on_key(tab, &Mode::Normal, ContentKind::Text).unwrap();
        assert_eq!(same.action, Action::Buffer(BufferCmd::Next), "Tab is the same key");

        let prev = input.on_key(ctrl('o'), &Mode::Normal, ContentKind::Text).unwrap();
        assert_eq!(prev.action, Action::Buffer(BufferCmd::Prev));
    }

    /// They reach the buffer list from a tree pane too, which is a request to
    /// show a buffer there — the same thing `:bn` means.
    #[test]
    fn the_buffer_keys_work_from_a_tree_as_well() {
        let mut input = Input::default();
        let next = input.on_key(ctrl('i'), &Mode::Normal, ContentKind::Tree).unwrap();
        assert_eq!(next.action, Action::Buffer(BufferCmd::Next));
    }

    /// The two keys under the `g` prefix that go somewhere rather than move
    /// the cursor. Both are sequences vim spends on something bi does not
    /// have, so neither cost anything to take.
    #[test]
    fn the_g_prefix_reaches_the_other_file_and_the_buffer_list() {
        assert_eq!(typed("ga").action, Action::Ex { line: "alt".into(), run: true });
        assert_eq!(typed("gf").action, Action::Buffer(BufferCmd::List));
    }

    /// A tree is where you look files up, so the key that looks one up by
    /// name has to reach it — and it must not fall through to `p`, which
    /// pastes into the directory under the cursor.
    #[test]
    fn ctrl_p_opens_the_file_picker_from_a_tree() {
        let mut input = Input::default();
        let cmd = input.on_key(ctrl('p'), &Mode::Normal, ContentKind::Tree).expect("resolved");
        assert_eq!(cmd.action, Action::OpenPicker(PickerKind::File));

        assert_eq!(tree_action("p"), Action::Tree(TreeCmd::Paste), "and a bare p still pastes");
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
        assert_eq!(
            cmd.action,
            Action::Move(Motion::Word { big: false, forward: true, end: false })
        );
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
                target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                count: 1,
                sink: Sink::Ring
            }
        );
    }

    #[test]
    fn indent_is_an_operator_like_any_other() {
        assert_eq!(
            typed(">j").action,
            Action::Operate {
                op: Operator::Indent { right: true },
                target: Target::Motion(Motion::Down),
                count: 1,
                sink: Sink::Ring
            }
        );
        assert_eq!(
            typed("<<").action,
            Action::Operate {
                op: Operator::Indent { right: false },
                target: Target::Motion(Motion::CurrentLine),
                count: 1,
                sink: Sink::Ring
            }
        );
        // The count folds into the range, as it does for `3dd`.
        assert_eq!(
            typed("3>>").action,
            Action::Operate {
                op: Operator::Indent { right: true },
                target: Target::Motion(Motion::CurrentLine),
                count: 3,
                sink: Sink::Ring
            }
        );
    }

    /// In visual mode the count is steps, not rows — the selection already
    /// says which rows — so it stays on the command.
    #[test]
    fn a_visual_indent_counts_steps() {
        let mut input = Input::default();
        let mut last = None;
        for c in "3>".chars() {
            last = input.on_key(key(c), &Mode::Visual(VisualKind::Char), ContentKind::Text);
        }
        let cmd = last.expect("indented");
        assert_eq!(
            cmd.action,
            Action::OperateSelection { op: Operator::Indent { right: true }, sink: Sink::Ring }
        );
        assert_eq!(cmd.count, 3);
    }

    #[test]
    fn tab_in_insert_mode_is_an_indent_rather_than_a_character() {
        let mut input = Input::default();
        let tab = |shift| Key {
            code: KeyCode::Tab,
            mods: crate::key::Mods { shift, ..Default::default() },
        };
        assert_eq!(
            input.on_key(tab(false), &Mode::Insert, ContentKind::Text).unwrap().action,
            Action::InsertIndent { right: true }
        );
        assert_eq!(
            input.on_key(tab(true), &Mode::Insert, ContentKind::Text).unwrap().action,
            Action::InsertIndent { right: false }
        );
    }

    // ---- surround -----------------------------------------------------------

    #[test]
    fn ys_takes_a_motion_and_then_a_character() {
        assert_eq!(
            typed("ysiw\"").action,
            Action::Surround {
                target: Target::Object { object: TextObject::Word { big: false }, around: false },
                count: 1,
                with: '"',
            }
        );
        assert_eq!(
            typed("ys2w)").action,
            Action::Surround {
                target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
                count: 2,
                with: ')',
            }
        );
    }

    /// The doubled form, exactly as `dd` is `d` doubled.
    #[test]
    fn yss_is_the_whole_line() {
        assert_eq!(
            typed("yss\"").action,
            Action::Surround { target: Target::Motion(Motion::CurrentLine), count: 1, with: '"' }
        );
    }

    #[test]
    fn ds_and_cs_take_their_characters_and_nothing_else() {
        assert_eq!(typed("ds\"").action, Action::Unsurround { of: '"' });
        assert_eq!(typed("cs\"'").action, Action::Resurround { of: '"', with: '\'' });
    }

    /// `(` is a motion nowhere and a surrounding here, which is the whole
    /// reason the pending character is read before the motion table.
    #[test]
    fn a_pending_surround_swallows_a_key_that_would_otherwise_move() {
        assert_eq!(typed("dsb").action, Action::Unsurround { of: 'b' });
        assert_eq!(
            typed("ysiwb").action,
            Action::Surround {
                target: Target::Object { object: TextObject::Word { big: false }, around: false },
                count: 1,
                with: 'b',
            }
        );
    }

    #[test]
    fn visual_s_wraps_the_selection() {
        let mut input = Input::default();
        let mut last = None;
        for c in "S\"".chars() {
            last = input.on_key(key(c), &Mode::Visual(VisualKind::Char), ContentKind::Text);
        }
        assert_eq!(last.unwrap().action, Action::SurroundSelection { with: '"' });
    }

    #[test]
    fn a_half_typed_surround_shows_in_the_pending_display() {
        let mut input = Input::default();
        input.on_key(key('c'), &Mode::Normal, ContentKind::Text);
        input.on_key(key('s'), &Mode::Normal, ContentKind::Text);
        assert_eq!(input.pending_display(), "cs");
        input.on_key(key('"'), &Mode::Normal, ContentKind::Text);
        assert_eq!(input.pending_display(), "cs\"");
    }

    #[test]
    fn cw_carries_the_change_operator() {
        assert_eq!(
            typed("cw").action,
            Action::Operate {
                op: Operator::Change,
                target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
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
                target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
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
                target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
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
                target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
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
        let ring = Sink::Ring;
        assert_eq!(typed("p").action, Action::Paste { before: false, count: 1, sink: ring });
        assert_eq!(typed("P").action, Action::Paste { before: true, count: 1, sink: ring });
        assert_eq!(typed("3p").action, Action::Paste { before: false, count: 3, sink: ring });
    }

    /// In visual mode `p` replaces the selection, so it must be claimed here
    /// rather than falling through to normal mode's put.
    #[test]
    fn p_over_a_selection_is_a_paste_of_its_own() {
        let visual = Mode::Visual(VisualKind::Char);
        let ring = Sink::Ring;
        let mut input = Input::default();

        let put = input.on_key(key('p'), &visual, ContentKind::Text).expect("resolved");
        assert_eq!(put.action, Action::PasteSelection { capture: true, count: 1, sink: ring });

        // `P` is the same paste, keeping the ring rather than swapping into it.
        let keep = input.on_key(key('P'), &visual, ContentKind::Text).expect("resolved");
        assert_eq!(keep.action, Action::PasteSelection { capture: false, count: 1, sink: ring });

        assert!(input.on_key(key('3'), &visual, ContentKind::Text).is_none(), "counting");
        let thrice = input.on_key(key('p'), &visual, ContentKind::Text).expect("resolved");
        assert_eq!(thrice.action, Action::PasteSelection { capture: true, count: 3, sink: ring });
    }

    /// The picker still owns `"p` in visual mode: there the `p` names the
    /// picker, and what it lands on is pasted over the selection anyway.
    #[test]
    fn quote_p_over_a_selection_still_opens_the_picker() {
        let visual = Mode::Visual(VisualKind::Char);
        let mut input = Input::default();
        assert!(input.on_key(key('"'), &visual, ContentKind::Text).is_none(), "armed");
        let cmd = input.on_key(key('p'), &visual, ContentKind::Text).expect("resolved");
        assert_eq!(cmd.action, Action::OpenPicker(PickerKind::Register { before: false }));
    }

    #[test]
    fn nothing_comes_out_of_the_black_hole_over_a_selection_either() {
        let visual = Mode::Visual(VisualKind::Line);
        let mut input = Input::default();
        assert!(input.on_key(key('"'), &visual, ContentKind::Text).is_none());
        assert!(input.on_key(key('_'), &visual, ContentKind::Text).is_none());
        assert!(input.on_key(key('p'), &visual, ContentKind::Text).is_none(), "and no delete");
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
                target: Target::Motion(Motion::Word { big: false, forward: true, end: false }),
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

    /// `"+` and `"*` are one register: X11's split between the clipboard and
    /// the primary selection is real, but OSC 52 addresses them with one code.
    #[test]
    fn the_system_register_reaches_both_the_operator_and_the_paste() {
        for spelling in ["\"+yy", "\"*yy"] {
            assert_eq!(
                typed(spelling).action,
                Action::Operate {
                    op: Operator::Yank,
                    target: Target::Motion(Motion::CurrentLine),
                    count: 1,
                    sink: Sink::System
                },
                "{spelling}"
            );
        }
        assert_eq!(
            typed("\"+p").action,
            Action::Paste { before: false, count: 1, sink: Sink::System }
        );
        assert_eq!(
            typed("\"+2P").action,
            Action::Paste { before: true, count: 2, sink: Sink::System }
        );
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

    /// The char arm below it would happily have typed a literal `r`, which is
    /// what it did before the history existed.
    #[test]
    fn ctrl_r_on_the_command_line_opens_the_history() {
        let mut input = Input::default();
        let line = Mode::Command("w".into());

        assert_eq!(
            input.on_key(ctrl('r'), &line, ContentKind::Text).unwrap().action,
            Action::OpenPicker(PickerKind::History),
        );
        assert_eq!(
            input.on_key(key('r'), &line, ContentKind::Text).unwrap().action,
            Action::CommandChar('r'),
            "without the ctrl it is still a letter",
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
            ('X', Operator::Delete, Motion::Left),
        ];
        // `s` was `cl` and is `s` now — see `docs/specs/find.md`. `cl` still
        // spells what it spelled.
        assert_eq!(typed("s").action, Action::EnterFind);
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
        assert_eq!(typed("X").action, typed("dh").action);
        // `S` was `cc` and is the scope picker now — see
        // `docs/specs/scopes.md`. `cc` still spells what it spelled.
        assert_eq!(typed("S").action, Action::ShowScopes);
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
