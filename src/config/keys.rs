//! The keymap: which key means which piece of bi's vocabulary.
//!
//! # What this is, and what it is not yet
//!
//! `docs/specs/config.md` describes a keymap where every binding is a
//! `Binding` in a per-mode trie, `input.rs` reads nothing else, and the
//! defaults live in `default.toml`. This is the first half of that: a name
//! resolves to the key that **already** produces it, and the user's key is
//! rewritten to that one on its way in.
//!
//! The consequence is worth being blunt about: you can rebind a key onto
//! anything bi already has a key for, and nothing else. `"j" = "left"` works.
//! A name with no default key — `git_blame`, or `word_end_backward`, which
//! vim spells as the two keys `ge` — cannot be a target yet, and says so at
//! load rather than failing silently.
//!
//! What it buys is that the whole grammar keeps working, untouched and
//! unrisked: rebinding `w` also rebinds `dw`, `d2w`, `c2w` and `vw`, because
//! by the time `input.rs` sees the key it *is* `w`. A trie in front of the
//! keymap would have had to re-implement counts, operator pending and the
//! four argument-taking states to get the same result.

use std::collections::HashMap;

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

/// A user's key rewritten onto bi's own, per mode.
///
/// `None` is an unbinding — `"h" = false` — and is why the value is an
/// `Option` rather than the key simply being absent from the map. A missing
/// key means "not mentioned, behave normally"; a present `None` means "this
/// key does nothing", and the two must not collapse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    maps: HashMap<KeyMode, HashMap<Key, Option<Key>>>,
}

impl Keymap {
    pub fn is_empty(&self) -> bool {
        self.maps.values().all(|m| m.is_empty())
    }

    pub fn insert(&mut self, mode: KeyMode, from: Key, to: Option<Key>) {
        self.maps.entry(mode).or_default().insert(from, to);
    }

    /// What `key` should be treated as in `mode`.
    ///
    /// `None` — the outer one — means the config said nothing about this key,
    /// so it keeps its built-in meaning. `Some(None)` means it was unbound.
    pub fn get(&self, mode: KeyMode, key: Key) -> Option<Option<Key>> {
        self.maps.get(&mode)?.get(&key).copied()
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

/// Every name a binding can have, and the key that already means it.
///
/// The single table `config.md` asks for, and the thing that turns
/// `unknown command: move_dwon` into a message rather than a silent no-op.
/// A name whose vim spelling is two keys — `ge`, `g_`, `gg` — is absent
/// deliberately: there is no single key to rewrite to, and claiming otherwise
/// would bind it to something wrong.
const NAMES: &[(KeyMode, &str, &str)] = &[
    // Motions.
    (KeyMode::Normal, "left", "h"),
    (KeyMode::Normal, "right", "l"),
    (KeyMode::Normal, "down", "j"),
    (KeyMode::Normal, "up", "k"),
    (KeyMode::Normal, "word_forward", "w"),
    (KeyMode::Normal, "word_backward", "b"),
    (KeyMode::Normal, "word_end", "e"),
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
];

/// The key `name` already means in `mode`.
///
/// Visual borrows normal's table: `input.rs` falls through to `normal` for
/// anything visual does not claim, so a motion rebound there has to reach the
/// same place.
pub fn key_for_name(mode: KeyMode, name: &str) -> Option<Key> {
    let lookup = if mode == KeyMode::Tree { KeyMode::Tree } else { KeyMode::Normal };
    let spelling = NAMES.iter().find(|(m, n, _)| *m == lookup && *n == name).map(|(_, _, k)| *k)?;
    parse_key(spelling).ok()
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
    fn every_name_in_the_table_resolves_to_a_key_it_spells() {
        for (mode, name, spelling) in NAMES {
            let key = key_for_name(*mode, name)
                .unwrap_or_else(|| panic!("{name} does not resolve in {mode:?}"));
            assert_eq!(key, parse_key(spelling).unwrap(), "{name}");
        }
    }

    #[test]
    fn a_name_is_looked_up_in_the_table_its_mode_uses() {
        assert_eq!(key_for_name(KeyMode::Normal, "left"), Some(Key::char('h')));
        // Visual borrows normal's, because `input.rs` falls through to it.
        assert_eq!(key_for_name(KeyMode::Visual, "left"), Some(Key::char('h')));
        assert_eq!(key_for_name(KeyMode::Tree, "tree_expand"), Some(Key::char('l')));
        // A tree name is not reachable from normal, nor the reverse.
        assert_eq!(key_for_name(KeyMode::Normal, "tree_expand"), None);
        assert_eq!(key_for_name(KeyMode::Tree, "left"), None);
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
        map.insert(KeyMode::Normal, Key::char('h'), None);
        map.insert(KeyMode::Normal, Key::char('j'), Some(Key::char('h')));

        assert_eq!(map.get(KeyMode::Normal, Key::char('h')), Some(None), "unbound");
        assert_eq!(map.get(KeyMode::Normal, Key::char('j')), Some(Some(Key::char('h'))));
        assert_eq!(map.get(KeyMode::Normal, Key::char('k')), None, "never mentioned");
        assert_eq!(map.get(KeyMode::Tree, Key::char('j')), None, "a different mode");
    }
}
