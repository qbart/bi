//! The keymap: which key means which piece of bi's vocabulary.
//!
//! # What this is, and what it is not yet
//!
//! `docs/specs/config.md` describes a keymap where every binding is a
//! `Binding` in a per-mode trie, `input.rs` reads nothing else, and the
//! defaults live in `default.toml`. This is most of that, short of `Binding`:
//! a name resolves to the keys that **already** produce it, and the user's
//! keys are rewritten to those on their way in.
//!
//! The consequence is worth being blunt about: you can rebind onto anything bi
//! already has keys for, and nothing else. `"j" = "left"` works, and so does
//! `"<leader>g" = "goto_first_line"` — Space then `g` arrives as `g` then `g`.
//! A name with no keys at all, `git_blame`, cannot be a target yet, and says
//! so at load rather than failing silently.
//!
//! What it buys is that the whole grammar keeps working, untouched and
//! unrisked: rebinding `w` also rebinds `dw`, `d2w`, `c2w` and `vw`, because
//! by the time `input.rs` sees the key it *is* `w`. A trie in front of the
//! keymap would have had to re-implement counts, operator pending and the
//! four argument-taking states to get the same result.

use std::collections::{HashMap, HashSet};

use crate::key::{Key, KeyCode, Mods};

/// Which map a key is looked up in. One per context `Input::on_key` already
/// dispatches on, so the config cannot name a mode the editor does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyMode {
    Normal,
    Visual,
    Tree,
}

impl KeyMode {
    /// The `[keys.<name>]` section this mode is written as.
    pub fn from_section(name: &str) -> Option<Self> {
        Some(match name {
            "normal" => Self::Normal,
            "visual" => Self::Visual,
            "tree" => Self::Tree,
            _ => return None,
        })
    }
}

/// What a sequence of typed keys means in one mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// A complete binding: feed these keys through the grammar instead.
    Keys(Vec<Key>),
    /// A complete binding to nothing — `"h" = false`. Swallowed, or `h` would
    /// still move left.
    Unbound,
    /// Not a binding, but the start of one. Waits for the next key.
    Prefix,
    /// Nothing here. The keys keep their built-in meaning.
    Miss,
}

/// A user's keys rewritten onto bi's own, per mode.
///
/// A binding is a sequence on both sides, which is what `<leader>e` needs on
/// the left and what `gg` needs on the right. `prefixes` holds every proper
/// prefix of every bound sequence, so "is this the start of something?" is a
/// lookup rather than a scan — the map-beside-a-prefix-set that
/// `docs/specs/config.md` says stands in for a trie until one measures.
///
/// `None` as a target is an unbinding — `"h" = false` — and is why the value
/// is an `Option` rather than the sequence simply being absent. A missing
/// sequence means "not mentioned, behave normally"; a present `None` means
/// "this does nothing", and the two must not collapse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    /// The key `<leader>` stands for. Expanded at parse time, so nothing
    /// downstream of the parser knows a leader was ever involved.
    leader: Option<Key>,
    maps: HashMap<KeyMode, Bindings>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Bindings {
    bound: HashMap<Vec<Key>, Option<Vec<Key>>>,
    prefixes: HashSet<Vec<Key>>,
}

impl Default for Keymap {
    /// `<Space>`, matching what `default.toml` ships. The two must agree, and
    /// a test says so — an embedder that never loads the shipped file still
    /// gets the documented leader.
    fn default() -> Self {
        Self { leader: Some(Key::char(' ')), maps: HashMap::new() }
    }
}

impl Keymap {
    /// Whether any key is bound at all. `leader` alone is not a binding: it
    /// changes what `<leader>` spells, and spells nothing on its own.
    pub fn is_empty(&self) -> bool {
        self.maps.values().all(|m| m.bound.is_empty())
    }

    pub fn leader(&self) -> Option<Key> {
        self.leader
    }

    pub fn set_leader(&mut self, key: Key) {
        self.leader = Some(key);
    }

    pub fn insert(&mut self, mode: KeyMode, from: Vec<Key>, to: Option<Vec<Key>>) {
        let map = self.maps.entry(mode).or_default();
        for len in 1..from.len() {
            map.prefixes.insert(from[..len].to_vec());
        }
        map.bound.insert(from, to);
    }

    /// What `keys` — everything typed since the last complete command — means.
    ///
    /// A complete binding wins over being a prefix of a longer one, which is
    /// the no-timeout rule: the longer one is unreachable, and the loader says
    /// so rather than a clock deciding at 500ms.
    pub fn lookup(&self, mode: KeyMode, keys: &[Key]) -> Lookup {
        let Some(map) = self.maps.get(&mode) else { return Lookup::Miss };
        match map.bound.get(keys) {
            Some(Some(to)) => Lookup::Keys(to.clone()),
            Some(None) => Lookup::Unbound,
            None if map.prefixes.contains(keys) => Lookup::Prefix,
            None => Lookup::Miss,
        }
    }

    /// Every bound sequence in `mode`, for the loader's unreachability check.
    pub fn sequences(&self, mode: KeyMode) -> impl Iterator<Item = &[Key]> {
        self.maps.get(&mode).into_iter().flat_map(|m| m.bound.keys().map(Vec::as_slice))
    }
}

/// A key written the way a config file writes it.
///
/// `"h"`, `";"`, `"<C-w>"`, `"<Esc>"`, `"<CR>"`, `"<Tab>"`, `"<Space>"`,
/// `"<BS>"`, and the four arrows. A bare character carries its own shift —
/// `"K"`, not `"<S-k>"` — which is how terminals report it and how
/// `KeyCode::Char` already stores it.
pub fn parse_key(spelling: &str) -> Result<Key, String> {
    let mut chars = spelling.chars();
    if let (Some(only), None) = (chars.next(), chars.next()) {
        return Ok(Key::char(only));
    }

    let inner = spelling
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .ok_or_else(|| format!("not a key: {spelling}"))?;

    let (mods, rest) = match inner.split_once('-') {
        Some(("C", rest)) => (Mods { ctrl: true, ..Mods::default() }, rest),
        Some(("A", rest)) => (Mods { alt: true, ..Mods::default() }, rest),
        Some(("S", rest)) => (Mods { shift: true, ..Mods::default() }, rest),
        _ => (Mods::default(), inner),
    };

    let code = match rest {
        "Esc" => KeyCode::Esc,
        "CR" | "Enter" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "BS" => KeyCode::Backspace,
        "Space" => KeyCode::Char(' '),
        // Vim's spelling, and the only way to write a literal `<` where one
        // would otherwise open a group — `"<lt>x"` is two keys, `"<x"` is
        // three only because nothing closes the group.
        "lt" => KeyCode::Char('<'),
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        one if one.chars().count() == 1 => KeyCode::Char(one.chars().next().unwrap()),
        _ => return Err(format!("not a key: {spelling}")),
    };
    Ok(Key { code, mods })
}

/// Keys written back the way a config file writes them.
///
/// The inverse of [`parse_keys`], for the two places that have keys and need
/// words: a diagnostic about a binding the user wrote as `<leader>e`, and the
/// status line showing a half-typed sequence. Both are clearer naming the key
/// that was actually resolved than echoing what was typed.
pub fn spell(keys: &[Key]) -> String {
    let mut out = String::new();
    for key in keys {
        let named = match key.code {
            KeyCode::Esc => "Esc",
            KeyCode::Enter => "CR",
            KeyCode::Tab => "Tab",
            KeyCode::Backspace => "BS",
            KeyCode::Left => "Left",
            KeyCode::Right => "Right",
            KeyCode::Up => "Up",
            KeyCode::Down => "Down",
            KeyCode::Home => "Home",
            KeyCode::End => "End",
            KeyCode::Char(' ') => "Space",
            KeyCode::Char('<') => "lt",
            KeyCode::Char(c) => {
                // A bare character carries its own shift, so only ctrl and alt
                // need a wrapper here.
                match (key.mods.ctrl, key.mods.alt) {
                    (true, _) => out.push_str(&format!("<C-{c}>")),
                    (_, true) => out.push_str(&format!("<A-{c}>")),
                    _ => out.push(c),
                }
                continue;
            }
        };
        let prefix = match (key.mods.ctrl, key.mods.alt, key.mods.shift) {
            (true, _, _) => "C-",
            (_, true, _) => "A-",
            (_, _, true) => "S-",
            _ => "",
        };
        out.push_str(&format!("<{prefix}{named}>"));
    }
    out
}

/// A binding's side: one key or a sequence of them.
///
/// `"<leader>gd"` is three keys, `"gg"` is two, `"h"` is one. `<leader>`
/// resolves here rather than downstream, so a stored binding carries no trace
/// of having been written with one — changing `leader` re-points every
/// binding that spells it, and nothing else has to know.
///
/// A `<…>` group that closes has to parse: `"<Esk>"` is a typo worth
/// reporting, not six keys. A `<` with nothing closing it is the literal key,
/// which is what `"<C-w><"` needs.
pub fn parse_keys(spelling: &str, leader: Option<Key>) -> Result<Vec<Key>, String> {
    let chars: Vec<char> = spelling.chars().collect();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let group_end = (chars[i] == '<')
            .then(|| chars[i + 1..].iter().position(|&c| c == '>').map(|at| i + 1 + at))
            .flatten();
        let Some(end) = group_end else {
            keys.push(Key::char(chars[i]));
            i += 1;
            continue;
        };
        let group: String = chars[i..=end].iter().collect();
        if group == "<leader>" {
            let key = leader.ok_or_else(|| {
                "<leader> is not set — add leader = \" \" under [keys]".to_string()
            })?;
            keys.push(key);
        } else {
            keys.push(parse_key(&group)?);
        }
        i = end + 1;
    }
    if keys.is_empty() {
        return Err("not a key: an empty binding".into());
    }
    Ok(keys)
}

/// Every name a binding can have, and the keys that already mean it.
///
/// The single table `config.md` asks for, and the thing that turns
/// `unknown command: move_dwon` into a message rather than a silent no-op.
/// A name whose vim spelling is two keys — `ge`, `g_`, `gg`, `<C-w>s` — is
/// here like any other: a target is a sequence, and its keys are fed through
/// the grammar one at a time, so `gg` reaches `Motion::FirstLine` through the
/// same `g_pending` that typing it does.
const NAMES: &[(KeyMode, &str, &str)] = &[
    // Motions.
    (KeyMode::Normal, "left", "h"),
    (KeyMode::Normal, "right", "l"),
    (KeyMode::Normal, "down", "j"),
    (KeyMode::Normal, "up", "k"),
    (KeyMode::Normal, "word_forward", "w"),
    (KeyMode::Normal, "word_backward", "b"),
    (KeyMode::Normal, "word_end", "e"),
    (KeyMode::Normal, "word_end_backward", "ge"),
    (KeyMode::Normal, "big_word_end_backward", "gE"),
    (KeyMode::Normal, "goto_first_line", "gg"),
    (KeyMode::Normal, "last_non_blank", "g_"),
    (KeyMode::Normal, "big_word_forward", "W"),
    (KeyMode::Normal, "big_word_backward", "B"),
    (KeyMode::Normal, "big_word_end", "E"),
    (KeyMode::Normal, "line_start", "0"),
    (KeyMode::Normal, "first_non_blank", "^"),
    (KeyMode::Normal, "line_end", "$"),
    (KeyMode::Normal, "last_line", "G"),
    (KeyMode::Normal, "matching_bracket", "%"),
    (KeyMode::Normal, "paragraph_forward", "}"),
    (KeyMode::Normal, "paragraph_backward", "{"),
    (KeyMode::Normal, "find_forward", "f"),
    (KeyMode::Normal, "find_backward", "F"),
    (KeyMode::Normal, "till_forward", "t"),
    (KeyMode::Normal, "till_backward", "T"),
    (KeyMode::Normal, "repeat_find", ";"),
    (KeyMode::Normal, "repeat_find_reverse", ","),
    // Operators.
    (KeyMode::Normal, "delete", "d"),
    (KeyMode::Normal, "change", "c"),
    (KeyMode::Normal, "yank", "y"),
    // Actions.
    (KeyMode::Normal, "insert", "i"),
    (KeyMode::Normal, "insert_after", "a"),
    (KeyMode::Normal, "insert_line_start", "I"),
    (KeyMode::Normal, "insert_line_end", "A"),
    (KeyMode::Normal, "open_below", "o"),
    (KeyMode::Normal, "open_above", "O"),
    (KeyMode::Normal, "visual", "v"),
    (KeyMode::Normal, "visual_line", "V"),
    (KeyMode::Normal, "undo", "u"),
    (KeyMode::Normal, "paste_after", "p"),
    (KeyMode::Normal, "paste_before", "P"),
    (KeyMode::Normal, "repeat", "."),
    (KeyMode::Normal, "replace_char", "r"),
    (KeyMode::Normal, "toggle_case", "~"),
    (KeyMode::Normal, "join_lines", "J"),
    (KeyMode::Normal, "register", "\""),
    (KeyMode::Normal, "command", ":"),
    (KeyMode::Normal, "search_forward", "/"),
    (KeyMode::Normal, "search_backward", "?"),
    (KeyMode::Normal, "search_next", "n"),
    (KeyMode::Normal, "search_prev", "N"),
    (KeyMode::Normal, "search_word_forward", "*"),
    (KeyMode::Normal, "search_word_backward", "#"),
    (KeyMode::Normal, "delete_char", "x"),
    (KeyMode::Normal, "delete_char_before", "X"),
    (KeyMode::Normal, "delete_to_line_end", "D"),
    (KeyMode::Normal, "change_to_line_end", "C"),
    (KeyMode::Normal, "change_line", "S"),
    (KeyMode::Normal, "substitute", "s"),
    // The window prefix. Two keys each, and a name apiece so a leader binding
    // can reach them — `<leader>e` for the tree is the one everybody writes.
    (KeyMode::Normal, "window_split", "<C-w>s"),
    (KeyMode::Normal, "window_vsplit", "<C-w>v"),
    (KeyMode::Normal, "window_close", "<C-w>c"),
    (KeyMode::Normal, "window_only", "<C-w>o"),
    (KeyMode::Normal, "window_tree", "<C-w>e"),
    (KeyMode::Normal, "window_cycle", "<C-w>w"),
    (KeyMode::Normal, "window_equalize", "<C-w>="),
    (KeyMode::Normal, "window_focus_left", "<C-w>h"),
    (KeyMode::Normal, "window_focus_down", "<C-w>j"),
    (KeyMode::Normal, "window_focus_up", "<C-w>k"),
    (KeyMode::Normal, "window_focus_right", "<C-w>l"),
    // The tree pane, whose keys are its own.
    (KeyMode::Tree, "tree_select_down", "j"),
    (KeyMode::Tree, "tree_select_up", "k"),
    (KeyMode::Tree, "tree_expand", "l"),
    (KeyMode::Tree, "tree_collapse", "h"),
    (KeyMode::Tree, "tree_open", "<CR>"),
    (KeyMode::Tree, "tree_root_up", "-"),
    (KeyMode::Tree, "tree_root_down", "+"),
    (KeyMode::Tree, "tree_last", "G"),
    (KeyMode::Tree, "tree_refresh", "R"),
    (KeyMode::Tree, "tree_yank_path", "y"),
    (KeyMode::Tree, "tree_copy", "c"),
    (KeyMode::Tree, "tree_cut", "x"),
    (KeyMode::Tree, "tree_paste", "p"),
    (KeyMode::Tree, "tree_clear_marks", "<Esc>"),
    (KeyMode::Tree, "tree_create", "a"),
    (KeyMode::Tree, "tree_rename", "r"),
    (KeyMode::Tree, "tree_delete", "dd"),
    // The window prefix works in a tree too — `input.rs` routes `<C-w>` there
    // before anything else — so its names have to be bindable here or a tree
    // pane could be opened by a leader binding and not closed by one.
    (KeyMode::Tree, "window_split", "<C-w>s"),
    (KeyMode::Tree, "window_vsplit", "<C-w>v"),
    (KeyMode::Tree, "window_close", "<C-w>c"),
    (KeyMode::Tree, "window_only", "<C-w>o"),
    (KeyMode::Tree, "window_tree", "<C-w>e"),
    (KeyMode::Tree, "window_cycle", "<C-w>w"),
    (KeyMode::Tree, "window_equalize", "<C-w>="),
    (KeyMode::Tree, "window_focus_left", "<C-w>h"),
    (KeyMode::Tree, "window_focus_down", "<C-w>j"),
    (KeyMode::Tree, "window_focus_up", "<C-w>k"),
    (KeyMode::Tree, "window_focus_right", "<C-w>l"),
    (KeyMode::Tree, "tree_first", "gg"),
    (KeyMode::Tree, "tree_toggle_hidden", "gh"),
];

/// The keys `name` already means in `mode`.
///
/// Visual borrows normal's table: `input.rs` falls through to `normal` for
/// anything visual does not claim, so a motion rebound there has to reach the
/// same place.
pub fn key_for_name(mode: KeyMode, name: &str) -> Option<Vec<Key>> {
    let lookup = if mode == KeyMode::Tree { KeyMode::Tree } else { KeyMode::Normal };
    let spelling = NAMES.iter().find(|(m, n, _)| *m == lookup && *n == name).map(|(_, _, k)| *k)?;
    // No name spells `<leader>`: these are bi's own keys, not a user's.
    parse_keys(spelling, None).ok()
}

/// Every name, as the `[keys.*]` sections that would bind it to what it
/// already does — the listing `bi config init` writes.
///
/// Generated from `NAMES` rather than kept as text beside it, because a second
/// copy of the keymap is a second copy to drift. Every line is a real binding
/// that would parse; `config init` comments them out, which is what makes the
/// file a menu rather than a replacement.
pub fn listing() -> String {
    let mut out = String::new();
    for (mode, section) in
        [(KeyMode::Normal, "normal"), (KeyMode::Visual, "visual"), (KeyMode::Tree, "tree")]
    {
        out.push_str(&format!("[keys.{section}]\n"));
        if mode == KeyMode::Visual {
            out.push_str(
                "# Visual falls back to [keys.normal] for everything it does not\n\
                 # claim, so a motion rebound there applies here too. Only put a\n\
                 # key here to make the two modes differ.\n",
            );
            continue;
        }
        let width = NAMES
            .iter()
            .filter(|(m, _, _)| *m == mode)
            .map(|(_, _, spelling)| spelling.len() + spelling.matches(['"', '\\']).count() + 2)
            .max()
            .unwrap_or(0);
        for (_, name, spelling) in NAMES.iter().filter(|(m, _, _)| *m == mode) {
            // `"` is a key — the register prefix — so the spelling has to be
            // escaped rather than dropped between quotes as it comes. Written
            // raw it produced `"""`, which is not TOML at all and took the
            // rest of the file with it.
            let key = format!("\"{}\"", spelling.replace('\\', "\\\\").replace('"', "\\\""));
            out.push_str(&format!("{key:width$} = \"{name}\"\n"));
        }
        out.push('\n');
    }
    out
}

/// The built-in sequences a binding on `bound` would make unreachable.
///
/// Binding anything that starts with `g` turns `g` into the user's prefix, and
/// a prefix has no meaning of its own — so `gg`, `ge`, `gE` and `g_` stop
/// resolving. The same goes for `<C-w>` and, in a tree, `d`. Derived from
/// `NAMES` rather than a list of bi's prefixes written out again here: the
/// multi-key names *are* that list, and one that cannot drift.
///
/// Not a reason to refuse the binding — it is exactly what a user rebinding
/// `g` asked for — but silence would be a trap, and every name here can be
/// bound back by name.
pub fn shadowed(mode: KeyMode, bound: &[Key]) -> Vec<&'static str> {
    let lookup = if mode == KeyMode::Tree { KeyMode::Tree } else { KeyMode::Normal };
    let Some(&first) = bound.first() else { return Vec::new() };
    NAMES
        .iter()
        .filter(|(m, _, _)| *m == lookup)
        .filter_map(|(_, name, spelling)| {
            let keys = parse_keys(spelling, None).ok()?;
            (keys.len() > 1 && keys[0] == first && keys != bound).then_some(*name)
        })
        .collect()
}

/// The closest name to `name`, for "did you mean".
///
/// A real edit distance rather than a cheaper guess: the first attempt here
/// scored on shared first letter and similar length, and confidently offered
/// `tree_paste` for `tree_expnd`. A suggestion that is wrong is worse than
/// none, so the answer is also dropped when it is not close — more than a
/// third of the name rewritten is a different name, not a typo.
pub fn nearest_name(mode: KeyMode, name: &str) -> Option<&'static str> {
    let lookup = if mode == KeyMode::Tree { KeyMode::Tree } else { KeyMode::Normal };
    let (best, distance) = NAMES
        .iter()
        .filter(|(m, _, _)| *m == lookup)
        .map(|(_, n, _)| (*n, edit_distance(name, n)))
        .min_by_key(|(_, d)| *d)?;
    (distance <= name.len().div_ceil(3).max(1)).then_some(best)
}

/// Levenshtein, two rows rather than a matrix. The names are short and this
/// runs once per bad line in a config file.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0; b.len() + 1];
    for (i, ac) in a.chars().enumerate() {
        row[0] = i + 1;
        for (j, bc) in b.iter().enumerate() {
            let cost = usize::from(ac != *bc);
            row[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1);
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_a_character_or_a_bracketed_name() {
        assert_eq!(parse_key("h"), Ok(Key::char('h')));
        assert_eq!(parse_key(";"), Ok(Key::char(';')));
        assert_eq!(parse_key("K"), Ok(Key::char('K')), "a bare char carries its own shift");
        assert_eq!(parse_key("<C-w>"), Ok(Key::ctrl('w')));
        assert_eq!(parse_key("<Esc>"), Ok(Key::code(KeyCode::Esc)));
        assert_eq!(parse_key("<CR>"), Ok(Key::code(KeyCode::Enter)));
        assert_eq!(parse_key("<Space>"), Ok(Key::char(' ')));
        assert_eq!(parse_key("<Down>"), Ok(Key::code(KeyCode::Down)));
        assert!(parse_key("gg").is_err(), "a sequence is not a key");
        assert!(parse_key("<Nope>").is_err());
    }

    #[test]
    fn a_sequence_splits_into_keys() {
        let leader = Some(Key::char(' '));
        assert_eq!(parse_keys("h", leader), Ok(vec![Key::char('h')]));
        assert_eq!(parse_keys("gg", leader), Ok(vec![Key::char('g'), Key::char('g')]));
        assert_eq!(parse_keys("<C-w>s", leader), Ok(vec![Key::ctrl('w'), Key::char('s')]));
        assert_eq!(
            parse_keys("<leader>gd", leader),
            Ok(vec![Key::char(' '), Key::char('g'), Key::char('d')])
        );
        assert_eq!(
            parse_keys("<leader><CR>", leader),
            Ok(vec![Key::char(' '), Key::code(KeyCode::Enter)])
        );
        // A `<` with nothing closing it is the key itself, which is what the
        // resize bindings need. `<lt>` is the way to write one that would
        // otherwise open a group.
        assert_eq!(parse_keys("<C-w><", leader), Ok(vec![Key::ctrl('w'), Key::char('<')]));
        assert_eq!(parse_keys("<lt>x", leader), Ok(vec![Key::char('<'), Key::char('x')]));
        // A group that closes has to parse: a typo is worth reporting, not
        // five keys that silently bind something else.
        assert!(parse_keys("<Esk>", leader).is_err());
        assert!(parse_keys("", leader).is_err());
    }

    #[test]
    fn leader_is_resolved_at_parse_time_and_says_so_when_unset() {
        assert_eq!(parse_keys("<leader>f", Some(Key::char('\\'))), {
            Ok(vec![Key::char('\\'), Key::char('f')])
        });
        let err = parse_keys("<leader>f", None).expect_err("no leader, no expansion");
        assert!(err.contains("leader"), "{err}");
    }

    #[test]
    fn every_name_in_the_table_resolves_to_the_keys_it_spells() {
        for (mode, name, spelling) in NAMES {
            let keys = key_for_name(*mode, name)
                .unwrap_or_else(|| panic!("{name} does not resolve in {mode:?}"));
            assert_eq!(keys, parse_keys(spelling, None).unwrap(), "{name}");
        }
    }

    #[test]
    fn a_name_is_looked_up_in_the_table_its_mode_uses() {
        assert_eq!(key_for_name(KeyMode::Normal, "left"), Some(vec![Key::char('h')]));
        // Visual borrows normal's, because `input.rs` falls through to it.
        assert_eq!(key_for_name(KeyMode::Visual, "left"), Some(vec![Key::char('h')]));
        assert_eq!(key_for_name(KeyMode::Tree, "tree_expand"), Some(vec![Key::char('l')]));
        // A tree name is not reachable from normal, nor the reverse.
        assert_eq!(key_for_name(KeyMode::Normal, "tree_expand"), None);
        assert_eq!(key_for_name(KeyMode::Tree, "left"), None);
        // The names that exist only because a target can be a sequence.
        assert_eq!(
            key_for_name(KeyMode::Normal, "goto_first_line"),
            Some(vec![Key::char('g'), Key::char('g')])
        );
        assert_eq!(
            key_for_name(KeyMode::Tree, "tree_delete"),
            Some(vec![Key::char('d'), Key::char('d')])
        );
    }

    #[test]
    fn a_typo_gets_a_suggestion() {
        assert_eq!(nearest_name(KeyMode::Normal, "lft"), Some("left"));
        assert_eq!(nearest_name(KeyMode::Normal, "word_forwrd"), Some("word_forward"));
        assert_eq!(nearest_name(KeyMode::Tree, "tree_expnd"), Some("tree_expand"));
        // Far from everything gets no guess rather than a confident wrong one.
        assert_eq!(nearest_name(KeyMode::Normal, "zzz"), None);
        assert_eq!(nearest_name(KeyMode::Normal, "git_blame"), None);
    }

    #[test]
    fn edit_distance_counts_single_character_edits() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("left", "left"), 0);
        assert_eq!(edit_distance("lft", "left"), 1, "one insertion");
        assert_eq!(edit_distance("lefy", "left"), 1, "one substitution");
        assert_eq!(edit_distance("leftt", "left"), 1, "one deletion");
        assert_eq!(edit_distance("", "left"), 4);
    }

    #[test]
    fn an_unbinding_is_not_the_same_as_an_absent_key() {
        let mut map = Keymap::default();
        map.insert(KeyMode::Normal, vec![Key::char('h')], None);
        map.insert(KeyMode::Normal, vec![Key::char('j')], Some(vec![Key::char('h')]));

        assert_eq!(map.lookup(KeyMode::Normal, &[Key::char('h')]), Lookup::Unbound);
        assert_eq!(
            map.lookup(KeyMode::Normal, &[Key::char('j')]),
            Lookup::Keys(vec![Key::char('h')])
        );
        assert_eq!(map.lookup(KeyMode::Normal, &[Key::char('k')]), Lookup::Miss, "never mentioned");
        assert_eq!(map.lookup(KeyMode::Tree, &[Key::char('j')]), Lookup::Miss, "a different mode");
    }

    #[test]
    fn a_sequence_is_a_prefix_until_it_is_complete() {
        let space = Key::char(' ');
        let mut map = Keymap::default();
        map.insert(KeyMode::Normal, vec![space, Key::char('e')], Some(vec![Key::ctrl('w')]));

        assert_eq!(map.lookup(KeyMode::Normal, &[space]), Lookup::Prefix);
        assert_eq!(
            map.lookup(KeyMode::Normal, &[space, Key::char('e')]),
            Lookup::Keys(vec![Key::ctrl('w')])
        );
        assert_eq!(map.lookup(KeyMode::Normal, &[space, Key::char('j')]), Lookup::Miss);
        // A prefix in one mode is not one in another.
        assert_eq!(map.lookup(KeyMode::Tree, &[space]), Lookup::Miss);
    }

    /// The no-timeout rule, from the map's side: a complete binding fires even
    /// when a longer one starts with it. The loader reports the longer as
    /// unreachable rather than a clock deciding between them.
    #[test]
    fn a_complete_binding_wins_over_being_a_prefix() {
        let g = Key::char('g');
        let mut map = Keymap::default();
        map.insert(KeyMode::Normal, vec![g], Some(vec![Key::char('h')]));
        map.insert(KeyMode::Normal, vec![g, Key::char('d')], Some(vec![Key::char('j')]));

        assert_eq!(map.lookup(KeyMode::Normal, &[g]), Lookup::Keys(vec![Key::char('h')]));
    }

    /// A round trip, because both users of `spell` — a diagnostic and the
    /// status line — are read by someone who will then type what they see.
    #[test]
    fn keys_spell_back_the_way_they_parse() {
        for spelling in ["h", "gg", "<C-w>s", "<Esc>", "<CR>", "<Space>e", "<lt>x", "<A-x>"] {
            let keys = parse_keys(spelling, None).expect(spelling);
            assert_eq!(parse_keys(&spell(&keys), None), Ok(keys), "{spelling}");
        }
        assert_eq!(spell(&[Key::char(' '), Key::char('e')]), "<Space>e");
        assert_eq!(spell(&parse_keys("<leader>e", Some(Key::char('\\'))).unwrap()), "\\e");
    }

    #[test]
    fn leader_defaults_to_space_and_is_not_a_binding() {
        let map = Keymap::default();
        assert_eq!(map.leader(), Some(Key::char(' ')));
        assert!(map.is_empty(), "a leader on its own binds nothing");
    }
}
