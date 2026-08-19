//! Short unique names for things you are about to press a key to reach.
//!
//! One machine, three hats: a letter on every window, a letter on every match
//! `s` found, a letter on every tree-sitter boundary `S` offers. All three
//! collect targets, name them, draw the names and read one or two keys back.
//!
//! See `docs/specs/labels.md`.

/// The keys labels are built from, best first.
///
/// Home row under the strongest fingers, then the rest of it, then the rows
/// above and below. A label is a key you press without looking, so the order
/// is about the hand and not about the alphabet.
pub const KEYS: &str = "fjdkslaghrueiwotyvnmcxzqpb";

/// `count` labels, avoiding every character in `exclude`.
///
/// Single characters while they last; past that, enough of the *worst* keys
/// become prefixes for two-character labels and the good keys stay single — so
/// what comes first keeps the one-key labels and nothing is typed twice until
/// there is no other way.
///
/// Fewer than `count` labels come back when two characters cannot name them
/// all, which takes 677 targets on one screen. The caller labels what it can
/// and leaves the rest unlabelled rather than pretending.
pub fn labels(count: usize, exclude: &[char]) -> Vec<String> {
    let keys: Vec<char> = KEYS.chars().filter(|c| !exclude.contains(c)).collect();
    let n = keys.len();
    if n == 0 || count == 0 {
        return Vec::new();
    }
    if count <= n {
        return keys.iter().take(count).map(|c| c.to_string()).collect();
    }

    // How many keys stay single: as many as possible, given that each one
    // given up to being a prefix is worth `n` labels.
    let mut singles = n;
    while singles > 0 && singles + (n - singles) * n < count {
        singles -= 1;
    }

    let mut out: Vec<String> = keys[..singles].iter().map(|c| c.to_string()).collect();
    'outer: for &prefix in &keys[singles..] {
        for &second in &keys {
            out.push(format!("{prefix}{second}"));
            if out.len() == count {
                break 'outer;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_labels_are_the_hand_and_not_the_alphabet() {
        assert_eq!(labels(3, &[]), ["f", "j", "d"]);
    }

    #[test]
    fn the_good_keys_stay_single_and_the_tail_doubles_up() {
        let n = KEYS.chars().count();
        let out = labels(n + 1, &[]);

        assert_eq!(out.len(), n + 1);
        assert_eq!(out[0], "f", "the front keeps its one-key label");
        assert!(out.iter().filter(|l| l.chars().count() == 1).count() >= n - 1);
        assert!(out.iter().any(|l| l.chars().count() == 2));

        let unique: std::collections::BTreeSet<&String> = out.iter().collect();
        assert_eq!(unique.len(), out.len(), "and every one of them is its own");
    }

    /// No label may *start* with an excluded character either, or a two-key
    /// label would swallow the keystroke that was meant to narrow the search.
    #[test]
    fn an_excluded_character_appears_nowhere() {
        let out = labels(200, &['f', 'j']);
        assert!(out.iter().all(|l| !l.contains('f') && !l.contains('j')));
        assert_eq!(out[0], "d");
    }

    #[test]
    fn nothing_to_label_is_no_labels() {
        assert!(labels(0, &[]).is_empty());
        // Every key excluded: there is nothing to press, so nothing is offered.
        let all: Vec<char> = KEYS.chars().collect();
        assert!(labels(5, &all).is_empty());
    }

    #[test]
    fn more_than_two_characters_can_name_comes_back_short_rather_than_wrong() {
        let n = KEYS.chars().count();
        let out = labels(n * n + 50, &[]);
        assert_eq!(out.len(), n * n);
        let unique: std::collections::BTreeSet<&String> = out.iter().collect();
        assert_eq!(unique.len(), out.len());
    }

    /// A single-character label must never be a prefix of a longer one, or
    /// pressing it would be ambiguous and need a timeout to resolve.
    #[test]
    fn no_label_is_the_start_of_another() {
        for count in [5, 27, 40, 200] {
            let out = labels(count, &[]);
            for a in &out {
                assert!(
                    !out.iter().any(|b| b != a && b.starts_with(a.as_str())),
                    "{a} prefixes another label at {count}"
                );
            }
        }
    }
}
